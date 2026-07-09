//! Pipeline and atlas management: frame end, buffer upload, glyph packing.

use super::*;

impl Renderer {
    /// Render the current frame to an offscreen texture and write it to `path`
    /// as a binary PPM (P6). Used for headless screenshot verification. When the
    /// CRT post-process is active the grid is rendered to the CRT scene texture
    /// and the composite pass writes the post-processed image to the capture
    /// target, so captures reflect the effect (GLASSY_CRT=1 verification).
    pub fn capture(&mut self, path: &std::path::Path) -> Result<()> {
        self.end_frame();
        let bg_count = self.bg_count;
        let fg_count = self.fg_count;
        let crt_scene = self.crt_active().then(|| self.crt_scene_view()).flatten();
        self.capture_with(path, |s, view, enc| match &crt_scene {
            Some(scene) => {
                s.record_passes(scene, enc, bg_count, fg_count);
                s.record_crt_pass(view, enc);
            }
            None => s.record_passes(view, enc, bg_count, fg_count),
        })
    }

    /// Capture the last-built MULTI-PANE frame to a PPM (headless verification of
    /// the split render path). Uploads the flat per-pane instance lists, then
    /// records the same scissored passes as `render_multi` into an offscreen
    /// target.
    pub fn capture_multi(&mut self, path: &std::path::Path) -> Result<()> {
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
        let crt_scene = self.crt_active().then(|| self.crt_scene_view()).flatten();
        self.capture_with(path, |s, view, enc| match &crt_scene {
            Some(scene) => {
                s.record_multi_passes(scene, enc);
                s.record_crt_pass(view, enc);
            }
            None => s.record_multi_passes(view, enc),
        })
    }

    /// Shared offscreen-capture machinery: allocate a render target, let `record`
    /// emit its passes into it, copy it back, and write a binary PPM (P6).
    pub(crate) fn capture_with(
        &mut self,
        path: &std::path::Path,
        record: impl FnOnce(&Self, &wgpu::TextureView, &mut wgpu::CommandEncoder),
    ) -> Result<()> {
        let width = self.config.width.max(1);
        let height = self.config.height.max(1);

        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // Readback rows must be padded to COPY_BYTES_PER_ROW_ALIGNMENT.
        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture-readback"),
            size: padded as u64 * height as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("capture-encoder"),
            });
        record(self, &view, &mut encoder);
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(encoder.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| anyhow::anyhow!("device poll failed: {e:?}"))?;
        match rx.recv() {
            Ok(Ok(())) => {}
            other => anyhow::bail!("buffer map failed: {other:?}"),
        }

        let data = slice.get_mapped_range();
        let bgra = matches!(
            self.config.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let mut out = Vec::with_capacity((width * height * 3) as usize + 32);
        out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
        for y in 0..height {
            let start = (y * padded) as usize;
            let row = &data[start..start + unpadded as usize];
            for px in row.chunks_exact(4) {
                if bgra {
                    out.extend_from_slice(&[px[2], px[1], px[0]]);
                } else {
                    out.extend_from_slice(&[px[0], px[1], px[2]]);
                }
            }
        }
        drop(data);
        readback.unmap();
        std::fs::write(path, out)?;
        Ok(())
    }

    // --- internals ---------------------------------------------------------

    /// Finalize the frame's instance data and upload only what changed.
    ///
    /// For each pass we compare this frame's per-row instance counts against the
    /// previous upload's layout (`*_row_offsets`):
    ///   * If the layout is identical, only the rows rebuilt this frame
    ///     (`dirty_rows`) are written, each as a small `write_buffer` sub-range —
    ///     the common per-frame case (a few rows of typing). Untouched rows are
    ///     left on the GPU as-is.
    ///   * If a row's count changed (shifting every later row), or the layout is
    ///     unknown (after `resize_grid`), we reflatten the whole grid and upload a
    ///     single contiguous range from the first divergent row to the end (rows
    ///     before it are byte-identical and already resident), growing the buffer
    ///     if needed.
    pub(crate) fn end_frame(&mut self) {
        self.bg_count = Self::flush_pass::<BgInstance>(
            &self.device,
            &self.queue,
            &self.rows,
            |r| &r.bg,
            &mut self.bg_flat,
            &mut self.bg_row_offsets,
            &mut self.bg_scratch_offsets,
            &self.dirty_rows,
            &mut self.bg_buffer,
            &mut self.bg_capacity,
            "bg-instances",
        );
        self.fg_count = Self::flush_pass::<FgInstance>(
            &self.device,
            &self.queue,
            &self.rows,
            |r| &r.fg,
            &mut self.fg_flat,
            &mut self.fg_row_offsets,
            &mut self.fg_scratch_offsets,
            &self.dirty_rows,
            &mut self.fg_buffer,
            &mut self.fg_capacity,
            "fg-instances",
        );
        // Image overlay: a small, fully-rebuilt instance set each frame. Grow the
        // buffer if needed, then upload the whole thing in one write.
        self.image_count = self.image_overlay.len() as u32;
        if !self.image_overlay.is_empty() {
            if self.image_overlay.len() > self.image_capacity {
                self.image_capacity = self.image_overlay.len().next_power_of_two();
                self.image_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("image-instances"),
                    size: (self.image_capacity * std::mem::size_of::<FgInstance>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            self.queue.write_buffer(
                &self.image_buffer,
                0,
                bytemuck::cast_slice(&self.image_overlay),
            );
        }
        // Panel overlay quads (modals / menus): same rebuild-every-frame strategy.
        self.overlay_count = self.overlay_quads.len() as u32;
        if !self.overlay_quads.is_empty() {
            if self.overlay_quads.len() > self.overlay_capacity {
                self.overlay_capacity = self.overlay_quads.len().next_power_of_two();
                self.overlay_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("overlay-instances"),
                    size: (self.overlay_capacity * std::mem::size_of::<BgInstance>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            self.queue.write_buffer(
                &self.overlay_buffer,
                0,
                bytemuck::cast_slice(&self.overlay_quads),
            );
        }
        // Panel text-on-glass glyphs: drawn after the overlay quads.
        self.overlay_text_count = self.overlay_text.len() as u32;
        if !self.overlay_text.is_empty() {
            if self.overlay_text.len() > self.overlay_text_capacity {
                self.overlay_text_capacity = self.overlay_text.len().next_power_of_two();
                self.overlay_text_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("overlay-text-instances"),
                    size: (self.overlay_text_capacity * std::mem::size_of::<FgInstance>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            self.queue.write_buffer(
                &self.overlay_text_buffer,
                0,
                bytemuck::cast_slice(&self.overlay_text),
            );
        }
        self.dirty_rows.clear();
    }

    /// Upload the overlay quad + text-on-glass buffers (rebuilt every frame) and
    /// set their draw counts. Shared by the single-pane `end_frame` path and the
    /// multi-pane `render_multi`/`capture_multi` paths so GUI chrome composites in
    /// both. Idempotent; safe to call when the lists are empty.
    pub(crate) fn upload_overlay_buffers(&mut self) {
        self.overlay_count = self.overlay_quads.len() as u32;
        if !self.overlay_quads.is_empty() {
            if self.overlay_quads.len() > self.overlay_capacity {
                self.overlay_capacity = self.overlay_quads.len().next_power_of_two();
                self.overlay_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("overlay-instances"),
                    size: (self.overlay_capacity * std::mem::size_of::<BgInstance>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            self.queue.write_buffer(
                &self.overlay_buffer,
                0,
                bytemuck::cast_slice(&self.overlay_quads),
            );
        }
        self.overlay_text_count = self.overlay_text.len() as u32;
        if !self.overlay_text.is_empty() {
            if self.overlay_text.len() > self.overlay_text_capacity {
                self.overlay_text_capacity = self.overlay_text.len().next_power_of_two();
                self.overlay_text_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("overlay-text-instances"),
                    size: (self.overlay_text_capacity * std::mem::size_of::<FgInstance>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            self.queue.write_buffer(
                &self.overlay_text_buffer,
                0,
                bytemuck::cast_slice(&self.overlay_text),
            );
        }
    }

    /// Upload a single instance pass (bg or fg), returning the total instance
    /// count for the draw call. See [`Renderer::end_frame`] for the strategy. The
    /// `pick` closure selects the per-row vector for the pass.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn flush_pass<T: bytemuck::Pod>(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rows: &[RowInstances],
        pick: impl Fn(&RowInstances) -> &Vec<T>,
        flat: &mut Vec<T>,
        offsets: &mut Vec<u32>,
        scratch: &mut Vec<u32>,
        dirty_rows: &[usize],
        buffer: &mut wgpu::Buffer,
        capacity: &mut usize,
        label: &str,
    ) -> u32 {
        let stride = std::mem::size_of::<T>();
        let n = rows.len();

        // Current layout: prefix sums of per-row counts (new_offsets[i] = first
        // instance index of row i; new_offsets[n] = total). Built into the
        // persistent `scratch` buffer to avoid a per-frame allocation.
        let new_offsets = scratch;
        new_offsets.clear();
        new_offsets.reserve(n + 1);
        let mut acc: u32 = 0;
        new_offsets.push(0);
        for r in rows {
            acc += pick(r).len() as u32;
            new_offsets.push(acc);
        }
        let total = acc as usize;

        // Fast path: the layout is unchanged from the last upload, so each row sits
        // at the same buffer offset. Upload only the rows rebuilt this frame.
        let layout_same = offsets.as_slice() == new_offsets.as_slice();
        if layout_same && total <= *capacity {
            for &row in dirty_rows {
                if row >= n {
                    continue;
                }
                let data = pick(&rows[row]);
                if data.is_empty() {
                    continue;
                }
                let byte_off = new_offsets[row] as u64 * stride as u64;
                queue.write_buffer(buffer, byte_off, bytemuck::cast_slice(data));
            }
            return total as u32;
        }

        // Slow path: a row's count shifted the layout (or it's unknown). Reflatten
        // and upload one contiguous range from the first divergent row onward.
        flat.clear();
        flat.reserve(total);
        for r in rows {
            flat.extend_from_slice(pick(r));
        }

        // Grow the buffer if needed (full re-upload then, from offset 0).
        let mut start_instance: usize = 0;
        if total > *capacity {
            let cap = total.next_power_of_two().max(INITIAL_INSTANCES);
            *buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (cap * stride) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            *capacity = cap;
        } else {
            // Buffer kept. We can skip a leading prefix of rows that are BOTH
            // positionally unchanged (their start offset matches the last upload)
            // AND not rebuilt this frame. The first row failing either condition is
            // where the resident bytes first diverge from `flat`.
            //
            // First positional divergence: the first index where the prefix offsets
            // stop matching. `offsets` may be a different length than `new_offsets`
            // (grid height change without a buffer grow), which `zip` handles by
            // stopping at the shorter; any remaining rows are treated as divergent.
            let pos_div = offsets
                .iter()
                .zip(new_offsets.iter())
                .take_while(|(a, b)| a == b)
                .count()
                .saturating_sub(1) // row index whose start offset first differs
                .min(n);
            // Earliest row rebuilt this frame (content may differ even at the same
            // offset), if any.
            let min_dirty = dirty_rows
                .iter()
                .copied()
                .filter(|&r| r < n)
                .min()
                .unwrap_or(n);
            let first_row = pos_div.min(min_dirty);
            start_instance = new_offsets[first_row] as usize;
        }

        if total > start_instance {
            let byte_off = (start_instance * stride) as u64;
            queue.write_buffer(
                buffer,
                byte_off,
                bytemuck::cast_slice(&flat[start_instance..]),
            );
        }

        // Adopt the new layout, keeping the old buffer as next frame's scratch.
        std::mem::swap(offsets, new_offsets);
        total as u32
    }

    /// Whether `ch` is routed to a dedicated family by `font_symbol_map`.
    ///
    /// Only the single-character path (`rasterize` → `pick_family_for`) honors
    /// this routing. Ligature-run shaping (`rasterize_run`) always shapes with
    /// the primary family, so a symbol-mapped character swept into a run would
    /// silently lose its routing and fall through to cosmic-text's own font
    /// cascade instead — which on macOS always includes Apple Color Emoji,
    /// producing a color glyph where the single-char path would have produced
    /// a flat one. Callers use this alongside [`Renderer::primary_font_covers`]
    /// to keep such characters out of runs — see `src/text/presentation.rs`
    /// for the full design this and the coverage check are two halves of.
    pub(crate) fn has_symbol_override(&self, ch: char) -> bool {
        crate::text::shape::lookup_symbol_family(&self.font_symbol_map, ch).is_some()
    }

    /// Whether the PRIMARY font actually has a glyph for `(ch, bold, italic)`
    /// — see `Text::primary_font_covers` for the full rationale (this is the
    /// half of the `presentation` policy that keeps an uncovered character out
    /// of ligature-run shaping in the first place, rather than reacting after
    /// the fact to whatever font cosmic-text's fallback cascade lands on).
    /// Cached per `(char, bold, italic)`, same convention as `glyph_cache`
    /// — a real font-coverage query is not something to repeat every frame for
    /// every cell in a hot render loop.
    pub(crate) fn primary_font_covers(&mut self, ch: char, bold: bool, italic: bool) -> bool {
        let key = (ch, bold, italic);
        if let Some(&covers) = self.primary_coverage_cache.get(&key) {
            return covers;
        }
        let covers = self.text.primary_font_covers(ch, bold, italic);
        self.primary_coverage_cache.insert(key, covers);
        covers
    }

    /// Ensure the glyph(s) for `(ch, bold, italic)` are rasterized and packed
    /// into the atlas, recording their `AtlasGlyph` entries in the cache.
    ///
    /// As a side-effect, if any glyph in the rasterized output has a pen advance
    /// greater than 1.1× the nominal cell width, the character is inserted into
    /// `wide_char_set` so `push_cell` can promote it to a 2-cell (WIDE) box.
    /// This corrects Nerd-font icons that are designed as 1.5× or 2× wide but
    /// are not flagged WIDE_CHAR by alacritty's unicode-width tables.
    pub(crate) fn ensure_glyphs(&mut self, ch: char, bold: bool, italic: bool) {
        let key = (ch, bold, italic);
        if self.glyph_cache.contains_key(&key) {
            return;
        }
        let raw = self.text.rasterize(ch, bold, italic);
        // Wide-icon detection: if any shaped glyph's pen advance exceeds
        // 1.1× the cell width, promote this character to the wide set.
        // We check the `advance` field populated by `build_glyphs`.
        let cell_w = self.metrics.width;
        let is_wide = raw.iter().any(|g| g.advance > cell_w * 1.1);
        if is_wide {
            self.wide_char_set.insert(key);
        }
        let rasters = collect_rasters(&raw);
        let packed = self.pack_rasters(&rasters);
        // If an atlas overflowed while packing, `packed` is missing the
        // overflowing glyph(s) — a truncated pack. Don't cache it: the
        // deferred repack at the next begin_frame will clear this cache
        // entry's slot anyway, so skip the insert and let the next lookup
        // recompute against the freshly-repacked atlas.
        if self.atlas_overflow_pending {
            return;
        }
        self.glyph_cache.insert(key, packed);
    }

    /// Like `ensure_glyphs`, but for a full grapheme cluster (combining/ZWJ
    /// sequence) shaped as a single unit.
    pub(crate) fn ensure_cluster_glyphs(&mut self, cluster: &str, bold: bool, italic: bool) {
        let key = (cluster.to_string(), bold, italic);
        if self.cluster_cache.contains_key(&key) {
            return;
        }
        let rasters = collect_rasters(&self.text.rasterize_cluster(cluster, bold, italic));
        let packed = self.pack_rasters(&rasters);
        // See ensure_glyphs: a truncated pack from an atlas overflow must not
        // be cached; the deferred repack next frame will make it correct.
        if self.atlas_overflow_pending {
            return;
        }
        self.cluster_cache.insert(key, packed);
    }

    /// Ensure that the ligature run `(text, bold, italic)` is shaped and packed
    /// into the atlas. The result is a `Vec<Vec<AtlasGlyph>>` — one inner Vec per
    /// input character — where only the *first* cell of each shaped glyph carries
    /// atlas entries and continuation cells have empty Vecs. This is stored in
    /// `ligature_run_cache` keyed by `(text, bold, italic)`.
    ///
    /// Callers must check `ligature_run_cache` after this call; the result is
    /// already there (or the cache was just populated).
    pub(crate) fn ensure_run_glyphs(&mut self, run: &str, bold: bool, italic: bool) {
        let key = (run.to_string(), bold, italic);
        if self.ligature_run_cache.contains_key(&key) {
            return;
        }
        let cell_w = self.metrics.width;
        let run_glyphs = self.text.rasterize_run(run, bold, italic, cell_w);
        // For each per-cell slot, collect_rasters and pack into the atlas.
        let mut per_cell: Vec<Vec<AtlasGlyph>> = Vec::with_capacity(run_glyphs.len());
        for slot in &run_glyphs {
            if slot.glyphs.is_empty() {
                per_cell.push(Vec::new());
            } else {
                let rasters = collect_rasters(&slot.glyphs);
                let packed = self.pack_rasters(&rasters);
                per_cell.push(packed);
            }
        }
        // See ensure_glyphs: if any slot's pack was truncated by an atlas
        // overflow, the whole run is stale — don't cache it, so the next
        // lookup recomputes it after the deferred repack.
        if self.atlas_overflow_pending {
            return;
        }
        self.ligature_run_cache.insert(key, per_cell);
    }

    /// Pack owned glyph bitmaps into the atlases, returning their placed entries.
    /// Coverage-mask glyphs go into the R8 mask atlas; color glyphs (emoji) go
    /// into the RGBA8 color atlas.
    ///
    /// If a packer fills, we do NOT clear caches or reset packers here — doing
    /// so mid-frame used to invalidate the atlas positions of instances already
    /// emitted earlier in *this* frame (their UVs would keep pointing at texels
    /// the reset packer then handed to someone else), which is what produced
    /// the emoji-scroll atlas corruption. Instead we skip the overflowing
    /// glyph for this call (it simply renders blank for one frame) and flag
    /// `atlas_overflow_pending` + `atlas_reset`; the actual clear + reset
    /// happens at the next [`Renderer::begin_frame`], before anything is
    /// packed or drawn, and the forced full rebuild repacks the skipped glyph.
    pub(crate) fn pack_rasters(&mut self, rasters: &[Raster]) -> Vec<AtlasGlyph> {
        let mut packed: Vec<AtlasGlyph> = Vec::with_capacity(rasters.len());
        let inv_mask = 1.0 / ATLAS_SIZE as f32;
        let inv_color = 1.0 / COLOR_ATLAS_SIZE as f32;
        for r in rasters {
            // Select the destination atlas, its packer, its uv scale, and the
            // source bytes-per-pixel for the upload.
            let (packer, texture, inv, bpp) = if r.is_color {
                (
                    &mut self.color_packer,
                    &self.color_atlas_texture,
                    inv_color,
                    4,
                )
            } else {
                (&mut self.packer, &self.mask_atlas_texture, inv_mask, 1)
            };
            let (x, y) = match packer.alloc(r.width, r.height) {
                Some(o) => o,
                None => {
                    log::debug!("glyph atlas full; deferring repack to next frame");
                    self.atlas_overflow_pending = true;
                    self.atlas_reset = true;
                    continue;
                }
            };
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &r.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(r.width * bpp),
                    rows_per_image: Some(r.height),
                },
                wgpu::Extent3d {
                    width: r.width,
                    height: r.height,
                    depth_or_array_layers: 1,
                },
            );
            packed.push(AtlasGlyph {
                uv_min: [x as f32 * inv, y as f32 * inv],
                uv_max: [(x + r.width) as f32 * inv, (y + r.height) as f32 * inv],
                px_w: r.width as f32,
                px_h: r.height as f32,
                left: r.left,
                top: r.top,
                is_color: r.is_color,
            });
        }
        packed
    }
}
