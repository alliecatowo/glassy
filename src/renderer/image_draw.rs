//! Inline-image rendering: lazy atlas allocation and per-frame image queuing.
//!
//! The image atlas (RGBA8, IMAGE_ATLAS_SIZE²) is allocated on the first
//! [`Renderer::draw_image`] call so text-only sessions never pay the ~4 MB
//! VRAM cost. The bind group is created alongside the texture and stored as
//! `Option<wgpu::BindGroup>` in the `Renderer`; `frame.rs` checks for `Some`
//! before issuing the image draw call.

use super::*;

impl Renderer {
    /// Lazily create the image atlas texture and its bind group on first use.
    /// Text-only sessions never call this; they never pay the 4 MB VRAM cost.
    pub(super) fn ensure_image_atlas(&mut self) {
        if self.image_atlas_texture.is_some() {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image-atlas"),
            size: wgpu::Extent3d {
                width: IMAGE_ATLAS_SIZE,
                height: IMAGE_ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image-bg"),
            layout: &self.image_atlas_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.image_atlas_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        });
        log::info!("renderer: image atlas allocated on first draw_image");
        self.image_atlas_texture = Some(texture);
        self.image_bind_group = Some(bind_group);
    }

    /// Queue an inline image to be drawn this frame at pixel rect
    /// `(x, y, dst_w, dst_h)`. `rgba` is straight-alpha RGBA8, `img_w`x`img_h`.
    /// The pixels are uploaded into the image atlas once per `id` and cached;
    /// subsequent frames only push a quad. Oversized images (larger than the
    /// atlas) are skipped.
    ///
    /// The image atlas and its bind group are created lazily here on the first
    /// call, saving ~4 MB of GPU VRAM at startup for sessions that never display
    /// inline images (the typical case: most terminal sessions are text-only).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_image(
        &mut self,
        id: u32,
        rgba: &[u8],
        img_w: u32,
        img_h: u32,
        x: f32,
        y: f32,
        dst_w: f32,
        dst_h: f32,
    ) {
        if img_w == 0 || img_h == 0 || rgba.len() < (img_w * img_h * 4) as usize {
            return;
        }

        // Lazy allocation: create the image atlas + bind group on first use.
        self.ensure_image_atlas();

        let glyph = match self.image_cache.get(&id).copied() {
            Some(g) => g,
            None => {
                let (px, py) = match self.image_packer.alloc(img_w, img_h) {
                    Some(o) => o,
                    None => {
                        // Atlas full: drop cached images and repack from scratch.
                        self.image_cache.clear();
                        self.image_packer.reset();
                        match self.image_packer.alloc(img_w, img_h) {
                            Some(o) => o,
                            None => return, // image larger than the atlas
                        }
                    }
                };
                // SAFETY: we just ensured image_atlas_texture is Some above.
                let texture = self.image_atlas_texture.as_ref().expect("lazy init");
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: px, y: py, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(img_w * 4),
                        rows_per_image: Some(img_h),
                    },
                    wgpu::Extent3d {
                        width: img_w,
                        height: img_h,
                        depth_or_array_layers: 1,
                    },
                );
                let inv = 1.0 / IMAGE_ATLAS_SIZE as f32;
                let g = AtlasGlyph {
                    uv_min: [px as f32 * inv, py as f32 * inv],
                    uv_max: [(px + img_w) as f32 * inv, (py + img_h) as f32 * inv],
                    px_w: img_w as f32,
                    px_h: img_h as f32,
                    left: 0,
                    top: 0,
                    is_color: true,
                };
                self.image_cache.insert(id, g);
                g
            }
        };
        self.image_overlay.push(FgInstance {
            pos: [x, y],
            size: [dst_w, dst_h],
            uv_min: glyph.uv_min,
            uv_max: glyph.uv_max,
            color: [1.0, 1.0, 1.0, 1.0],
            flags: 1,
            _pad: [0; 3],
        });
    }
}
