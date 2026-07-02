//! CRT / glow / scanline post-process (config `crt_effect`, default OFF).
//!
//! When the effect is OFF this module is entirely dormant: `CrtPass` holds only
//! `None`s, no offscreen texture or pipeline is created, and the render path
//! takes the ordinary direct-to-surface route. The default build therefore pays
//! ZERO extra GPU cost and the idle/memory benchmarks are untouched.
//!
//! When ON, the grid (bg + fg + images + overlays) is rendered to an offscreen
//! RGBA texture, then a single fullscreen triangle (`vs_crt`/`fs_crt` in
//! shader.wgsl) samples that texture and composites it to the surface with
//! barrel curvature, scanlines, an aperture-grille tint, a cheap glow, and a
//! vignette. The effect is static (no animation), so it adds no redraw wakeups —
//! it only repaints once when toggled and on the normal damage-driven frames.

use super::*;

/// CRT / window-effect parameter uniform, mirrored from the WGSL `CrtU`.
/// `params  = [curvature, scanline, glow, vignette]`.
/// `params2 = [grain, tint, reserved, reserved]` — the extra channels so the
/// `Custom` effect can stack grain + glass-tint on top of the first four.
/// `mode`   = the [`super::effect::WindowEffect`] shader discriminant; `.yzw`
/// are padding. 48 bytes total (16-byte aligned for WGSL).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CrtUniform {
    params: [f32; 4],
    params2: [f32; 4],
    mode: [u32; 4],
}

/// Lazily-allocated post-process resources. Everything is `Option`/empty until
/// the effect is enabled AND a non-1×1 size is known, so an off session never
/// allocates the offscreen target.
#[derive(Default)]
pub(crate) struct CrtPass {
    /// Whether the effect is enabled (config `crt_effect`).
    enabled: bool,
    /// The post pipeline (built once on first enable; reused thereafter).
    pipeline: Option<wgpu::RenderPipeline>,
    /// Bind group layout for group(2): scene texture + sampler + params uniform.
    bgl: Option<wgpu::BindGroupLayout>,
    /// Linear sampler for the scene texture (smooth curvature/glow taps).
    sampler: Option<wgpu::Sampler>,
    /// The CRT parameter uniform buffer + its current values.
    params_buffer: Option<wgpu::Buffer>,
    params: [f32; 4],
    /// Extra effect channels `[grain, tint, _, _]` (Custom mode).
    params2: [f32; 4],
    /// The active effect's shader-mode discriminant (see
    /// [`super::effect::WindowEffect::shader_mode`]). 1 = the classic CRT look,
    /// which is what `set_crt(true)` selects for backward compatibility.
    mode: u32,
    /// The offscreen scene target + its bind group, sized to the surface. Both
    /// are recreated whenever the surface size changes.
    scene_texture: Option<wgpu::Texture>,
    scene_bind_group: Option<wgpu::BindGroup>,
    /// The size the current `scene_texture` was allocated for, so resize knows
    /// whether to reallocate.
    size: (u32, u32),
}

impl CrtPass {
    /// The current CRT parameters `[curvature, scanline, glow, vignette]`.
    #[allow(dead_code)]
    pub fn params(&self) -> [f32; 4] {
        self.params
    }
}

impl Renderer {
    /// Enable or disable the CRT post-process at runtime. Enabling lazily builds
    /// the post pipeline + offscreen target on the next frame; disabling drops
    /// nothing heavy immediately (the GPU resources are cheap to keep) but the
    /// render path reverts to direct-to-surface so there is no per-frame cost.
    /// The caller should force a full repaint after toggling so the change shows.
    #[allow(dead_code)] // retained legacy API; app now drives via set_window_effect
    pub fn set_crt(&mut self, enabled: bool) {
        // Classic CRT == the `WindowEffect::Crt` mode + params (single source of
        // truth). Delegate to the unified mode setter so the CRT bool stays a thin
        // wrapper over the window-effect machinery.
        let crt = super::effect::WindowEffect::Crt;
        self.set_crt_mode(enabled, crt.shader_mode(), crt.params(), crt.params2());
    }

    /// Unified entry point for every window-effect mode (CRT included). `active`
    /// gates the offscreen post pass; `mode` is the `fs_crt` shader discriminant;
    /// `params` is `[curvature, scanline, glow, vignette]` and `params2` is
    /// `[grain, tint, _, _]`. When `active` is false the render path reverts to
    /// direct-to-surface (zero per-frame cost). When true it lazily builds the
    /// post pipeline + offscreen target and pushes the new params + mode to the
    /// uniform. Idempotent for an unchanged selection.
    pub(crate) fn set_crt_mode(
        &mut self,
        active: bool,
        mode: u32,
        params: [f32; 4],
        params2: [f32; 4],
    ) {
        // Nothing to do if the selection is unchanged.
        if active == self.crt.enabled
            && (!active
                || (self.crt.mode == mode
                    && self.crt.params == params
                    && self.crt.params2 == params2))
        {
            return;
        }
        self.crt.enabled = active;
        if active {
            self.crt.params = params;
            self.crt.params2 = params2;
            self.crt.mode = mode;
            self.ensure_crt_resources();
        }
    }

    /// Whether the CRT effect is currently active. Public query for a settings
    /// UI / live-reload toggle to read the current state.
    #[allow(dead_code)]
    pub fn crt_enabled(&self) -> bool {
        self.crt.enabled
    }

    /// Build the post pipeline + sampler + params uniform if not yet created.
    /// Idempotent; cheap to call every enable. The offscreen scene texture is
    /// allocated separately in [`Renderer::ensure_crt_target`] (size-dependent).
    fn ensure_crt_resources(&mut self) {
        if self.crt.pipeline.is_some() {
            // Pipeline exists; just refresh the params uniform.
            self.upload_crt_params();
            return;
        }
        let device = &self.device;

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("crt-bgl"),
            entries: &[
                // binding 0: offscreen scene texture.
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // binding 1: linear sampler.
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // binding 2: CRT parameter uniform.
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("crt-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("crt-params"),
            size: std::mem::size_of::<CrtUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // The post pass reads the shared screen-size uniform (group 0) for the
        // resolution and group(2) for the scene + params.
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("crt-pl"),
            bind_group_layouts: &[Some(&self.uniform_bind_group_layout), None, Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("crt-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &self.crt_shader,
                entry_point: Some("vs_crt"),
                buffers: &[], // fullscreen triangle from vertex_index
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.crt_shader,
                entry_point: Some("fs_crt"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.config.format,
                    // Opaque composite: the post pass produces the final image
                    // and overwrites the surface. Alpha=1 keeps the window opaque
                    // where the CRT bezel/black borders are (curvature corners).
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: self.pipeline_cache.as_ref(),
        });

        self.crt.bgl = Some(bgl);
        self.crt.sampler = Some(sampler);
        self.crt.params_buffer = Some(params_buffer);
        self.crt.pipeline = Some(pipeline);
        self.upload_crt_params();
        self.ensure_crt_target();
    }

    /// Push the current CRT parameters to the uniform buffer.
    fn upload_crt_params(&self) {
        if let Some(buf) = &self.crt.params_buffer {
            self.queue.write_buffer(
                buf,
                0,
                bytemuck::bytes_of(&CrtUniform {
                    params: self.crt.params,
                    params2: self.crt.params2,
                    mode: [self.crt.mode, 0, 0, 0],
                }),
            );
        }
    }

    /// (Re)allocate the offscreen scene texture + its bind group to the current
    /// surface size if needed. No-op when the size is unchanged or the effect is
    /// off / resources not yet built.
    fn ensure_crt_target(&mut self) {
        if !self.crt.enabled || self.crt.pipeline.is_none() {
            return;
        }
        let (w, h) = (self.config.width.max(1), self.config.height.max(1));
        if self.crt.scene_texture.is_some() && self.crt.size == (w, h) {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("crt-scene"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Match the surface format so the grid passes (which target the
            // surface format) render into it byte-identically, then the post pass
            // reads it back.
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("crt-scene-bg"),
            layout: self.crt.bgl.as_ref().expect("crt bgl built with pipeline"),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(
                        self.crt.sampler.as_ref().expect("crt sampler built"),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self
                        .crt
                        .params_buffer
                        .as_ref()
                        .expect("crt params built")
                        .as_entire_binding(),
                },
            ],
        });
        self.crt.scene_texture = Some(texture);
        self.crt.scene_bind_group = Some(bind_group);
        self.crt.size = (w, h);
    }

    /// Hook from `resize()`: keep the offscreen target sized to the surface.
    pub(crate) fn crt_on_resize(&mut self) {
        self.ensure_crt_target();
    }

    /// Whether the render path should route through the offscreen + post pass.
    /// True only when enabled AND all resources are ready.
    pub(crate) fn crt_active(&self) -> bool {
        self.crt.enabled && self.crt.scene_bind_group.is_some()
    }

    /// An offscreen [`wgpu::TextureView`] for the scene target (the grid passes
    /// render here when CRT is active). `None` when the target isn't allocated.
    pub(crate) fn crt_scene_view(&self) -> Option<wgpu::TextureView> {
        self.crt
            .scene_texture
            .as_ref()
            .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()))
    }

    /// Record the fullscreen CRT composite pass: sample the offscreen scene and
    /// write the post-processed image to `surface_view`. Caller guarantees
    /// `crt_active()`.
    pub(crate) fn record_crt_pass(
        &self,
        surface_view: &wgpu::TextureView,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let (Some(pipeline), Some(scene_bg)) = (
            self.crt.pipeline.as_ref(),
            self.crt.scene_bind_group.as_ref(),
        ) else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("crt-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: surface_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        pass.set_bind_group(2, scene_bg, &[]);
        pass.draw(0..3, 0..1);
    }
}
