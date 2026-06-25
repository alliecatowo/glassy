//! Renderer initialization, resize, and accessor methods.

use super::*;

impl Renderer {
    pub fn new(
        window: Arc<Window>,
        font_family: Option<String>,
        font_px: f32,
        opacity: f32,
        font_features: Vec<String>,
    ) -> Result<Renderer> {
        let t = std::time::Instant::now();
        let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;

        // --- Parallel init: font load on main thread, GPU init on a side thread. ---
        //
        // wgpu surface creation must happen on the main thread (it borrows the
        // window handle), but the async adapter + device requests are pure GPU work
        // with no main-thread constraint.  We therefore:
        //   1. Create the wgpu Instance + Surface on the main thread (fast).
        //   2. Spawn a thread that requests the adapter + device via pollster.
        //   3. Call Text::load on the main thread (font scan, ~30-200 ms on a cold run).
        //   4. Join the GPU thread — if it finished while fonts loaded, join is free.
        //
        // On a typical laptop this shaves 50-150 ms off startup latency because the
        // Vulkan driver initialisation and font discovery overlap instead of stacking.

        // `InstanceDescriptor` has no `Default` in wgpu 29 (its `display` field is
        // non-defaultable), so build it via the explicit constructor.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(window.clone())
            .context("creating wgpu surface")?;

        // Default to the integrated/low-power GPU: a 2D glyph renderer never needs
        // the dGPU, and on a hybrid laptop HighPerformance wakes NVIDIA (5-7 W idle
        // pre-Turing, plus per-process driver RSS). Override with GLASSY_GPU=high or
        // GLASSY_GPU=discrete for the discrete GPU (also set WGPU_ADAPTER_NAME=<name>
        // and PRIME vars on Optimus: __NV_PRIME_RENDER_OFFLOAD=1 __VK_LAYER_NV_optimus=NVIDIA_only).
        let power_preference = match std::env::var("GLASSY_GPU").ok().as_deref() {
            Some("high") | Some("discrete") => wgpu::PowerPreference::HighPerformance,
            Some("low") | Some("integrated") => wgpu::PowerPreference::LowPower,
            _ => wgpu::PowerPreference::LowPower, // default: iGPU / integrated
        };

        // Snapshot the surface's raw handle for the compatible_surface check inside
        // the thread.  wgpu::Surface is Send; we pass it via an Arc so both sides
        // can observe the same surface without moving ownership into the thread.
        let surface_arc = Arc::new(surface);
        let surface_arc_thread = surface_arc.clone();

        // Spawn the GPU init thread. It requests the adapter (GPU selection) and
        // then the logical device; both are async-over-pollster and CPU-bound
        // (driver IPC + validation layer init).
        let gpu_thread = std::thread::spawn(
            move || -> anyhow::Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
                let adapter =
                    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference,
                        force_fallback_adapter: false,
                        compatible_surface: Some(surface_arc_thread.as_ref()),
                    }))
                    .context("requesting GPU adapter")?;
                // Request PIPELINE_CACHE if the adapter supports it (Vulkan only today).
                // We leave all other features/limits at defaults so the device request never
                // fails on a feature-limited adapter.
                let supports_pipeline_cache =
                    adapter.features().contains(wgpu::Features::PIPELINE_CACHE);
                let required_features = if supports_pipeline_cache {
                    wgpu::Features::PIPELINE_CACHE
                } else {
                    wgpu::Features::empty()
                };
                let (device, queue) =
                    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                        label: Some("glassy"),
                        required_features,
                        // Keep limits at their default (adapter-reported) values — over-tight
                        // limits can make request_device fail (e.g. max_texture_dimension_2d
                        // below the surface size, or wrong max_vertex_attributes for the fg
                        // layout). The big win is memory_hints, which avoids the Vulkan
                        // sub-allocator pre-reserving large block pools.
                        required_limits: wgpu::Limits::default(),
                        // MemoryUsage tells the wgpu Vulkan/Metal sub-allocators to prefer
                        // smaller pool blocks and release memory eagerly — the single biggest
                        // idle VRAM reduction without changing render behavior.
                        memory_hints: wgpu::MemoryHints::MemoryUsage,
                        ..Default::default() // experimental_features + trace
                    }))
                    .context("requesting GPU device")?;
                Ok((adapter, device, queue))
            },
        );

        // Font load runs concurrently with the GPU thread above.
        let (text, metrics) = Text::load(font_family.as_deref(), font_px, &font_features)
            .context("loading font and cell metrics")?;
        log::info!("  renderer: font loaded {:.1} ms", ms(t));

        // Recover the surface from the Arc now that the thread is done (or about to
        // be done — join() ensures it has released its clone before we unwrap).
        // The thread holds surface_arc_thread; after join it is dropped.
        let (adapter, device, queue) = gpu_thread
            .join()
            .map_err(|e| anyhow::anyhow!("GPU init thread panicked: {e:?}"))?
            .context("GPU init failed")?;
        log::info!("  renderer: GPU device ready {:.1} ms", ms(t));

        // Recover surface from the Arc — safe because the thread clone was dropped
        // by join().
        let surface = Arc::try_unwrap(surface_arc)
            .map_err(|_| anyhow::anyhow!("surface Arc still has multiple owners after join"))?;

        {
            // Log which GPU/backend we actually selected — confirms a real
            // device (vs the llvmpipe/lavapipe software fallback) for benchmarking.
            let info = adapter.get_info();
            log::info!(
                "glassy GPU: {} | backend={:?} | type={:?} | driver={}",
                info.name,
                info.backend,
                info.device_type,
                info.driver
            );
        }

        // --- Pipeline cache: load stored bytes from $XDG_CACHE_HOME/glassy/. ---
        // Gated on PIPELINE_CACHE feature (Vulkan only). On unsupported backends
        // we get None and all three pipelines pass `cache: None` — identical to
        // before this change. The cache bytes are saved at program exit via
        // Renderer::save_pipeline_cache(); the caller is responsible for that call.
        let adapter_info = adapter.get_info();
        let pipeline_cache: Option<wgpu::PipelineCache> =
            if device.features().contains(wgpu::Features::PIPELINE_CACHE) {
                let cache_data = load_pipeline_cache_data(&adapter_info);
                // SAFETY: the data bytes came from a previous PipelineCache::get_data()
                // call (or are None).  wgpu validates the data and falls back to an
                // empty cache (fallback: true) if it is stale/corrupt.
                let cache = unsafe {
                    device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
                        label: Some("glassy-pipeline-cache"),
                        data: cache_data.as_deref(),
                        fallback: true,
                    })
                };
                Some(cache)
            } else {
                None
            };

        // --- Surface format / present-mode selection. ---
        let caps = surface.get_capabilities(&adapter);
        // Prefer a standard 8-bit UNORM format so the capture() PPM readback (which
        // assumes 8-bit BGRA/RGBA) stays correct; some adapters offer a 10-bit packed
        // format (e.g. Rgb10a2Unorm) as the first non-srgb option, which renders fine
        // on screen but breaks the 8-bit readback.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
            .or_else(|| {
                caps.formats
                    .iter()
                    .copied()
                    .find(|f| *f == wgpu::TextureFormat::Rgba8Unorm)
            })
            .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
            .or_else(|| caps.formats.first().copied())
            .context("GPU adapter reported no compatible surface formats")?;
        // Fifo (vsync, guaranteed-available) is correct for an event-driven terminal:
        // it keeps the minimum swapchain images (2 vs Mailbox's typical 3, saving
        // ~8 MB at 1080p Bgra8), never redraws idle frames, and avoids the tearing
        // / latency tradeoffs of Mailbox/Immediate that are meaningless for a glyph app.
        let present_mode = wgpu::PresentMode::Fifo;

        // Window translucency: prefer PreMultiplied (Vulkan/Linux) where the
        // surface stores premultiplied RGBA and the compositor blends it directly.
        // Fall back to PostMultiplied (Metal/macOS) which uses straight alpha —
        // the compositor premultiplies before blending, so we must NOT premultiply
        // the RGB channels ourselves. Either mode produces a translucent window;
        // only the premultiplication convention differs. If neither is available
        // the compositor can't do translucency and we stay fully opaque.
        let (transparent, premultiplied_surface, alpha_mode) = if caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            (true, true, wgpu::CompositeAlphaMode::PreMultiplied)
        } else if caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PostMultiplied)
        {
            (true, false, wgpu::CompositeAlphaMode::PostMultiplied)
        } else {
            (
                false,
                false,
                caps.alpha_modes
                    .first()
                    .copied()
                    .unwrap_or(wgpu::CompositeAlphaMode::Auto),
            )
        };

        // Surface stays unconfigured until `resize()`; start at 1x1 as a placeholder.
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: 1,
            height: 1,
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
        };

        // --- Static unit quad: triangle-strip order (0,0)(1,0)(0,1)(1,1). ---
        let quad: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];
        let unit_quad = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("unit-quad"),
            contents: bytemuck::cast_slice(&quad),
            usage: wgpu::BufferUsages::VERTEX,
        });

        // --- group(0): screen-size uniform. ---
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("uniform"),
            contents: bytemuck::bytes_of(&Uniform {
                screen: [1.0, 1.0, 0.0, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("uniform-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniform-bg"),
            layout: &uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        // --- group(1): glyph atlas textures (R8 mask + RGBA8 color) + sampler. ---
        let mask_atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph-mask-atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mask_atlas_view =
            mask_atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let color_atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph-color-atlas"),
            size: wgpu::Extent3d {
                width: COLOR_ATLAS_SIZE,
                height: COLOR_ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let color_atlas_view =
            color_atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let atlas_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("atlas-bgl"),
                entries: &[
                    // binding 0: R8 mask atlas.
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
                    // binding 1: shared sampler.
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    // binding 2: RGBA8 color atlas.
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            });
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas-bg"),
            layout: &atlas_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&mask_atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&atlas_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&color_atlas_view),
                },
            ],
        });

        // Dedicated image atlas + bind group: lazily allocated on first draw_image.
        // Most terminal sessions never display inline images, so we skip the 4 MB
        // GPU texture at startup. The atlas_bind_group_layout and atlas_sampler are
        // kept in the Renderer struct so the bind group can be created later without
        // re-creating the layout (which requires device access at that point).
        //
        // `atlas_sampler` is moved into the Renderer struct below; `atlas_bind_group_layout`
        // is cloned into it as well (BindGroupLayout is Arc-backed and cheap to clone).
        // The image_atlas_texture and image_bind_group fields start as None.

        // --- Shader + pipelines. ---
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glassy-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shader.wgsl").into()),
        });

        // Vertex layouts: slot 0 = unit quad (per-vertex), slot 1 = instances.
        let quad_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2],
        };
        let bg_instance_attrs = wgpu::vertex_attr_array![
            1 => Float32x2, // pos
            2 => Float32x2, // size
            3 => Float32x4, // color
        ];
        let bg_instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<BgInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &bg_instance_attrs,
        };
        let fg_instance_attrs = wgpu::vertex_attr_array![
            1 => Float32x2, // pos
            2 => Float32x2, // size
            3 => Float32x2, // uv_min
            4 => Float32x2, // uv_max
            5 => Float32x4, // color
            6 => Uint32,    // flags
        ];
        let fg_instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<FgInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &fg_instance_attrs,
        };

        let bg_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bg-pl"),
            bind_group_layouts: &[Some(&uniform_bind_group_layout)],
            immediate_size: 0,
        });
        let fg_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fg-pl"),
            bind_group_layouts: &[
                Some(&uniform_bind_group_layout),
                Some(&atlas_bind_group_layout),
            ],
            immediate_size: 0,
        });

        let bg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("bg-pipeline"),
            layout: Some(&bg_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_bg"),
                buffers: &[quad_layout.clone(), bg_instance_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_bg"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: pipeline_cache.as_ref(),
        });

        let fg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fg-pipeline"),
            layout: Some(&fg_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fg"),
                buffers: &[quad_layout, fg_instance_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_fg"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Premultiplied blending so glyphs composite correctly over a
                    // translucent backdrop (and identically over an opaque one):
                    // the shader emits premultiplied color, so dst is weighted by
                    // (1 - src.a). The RGB blend is correct as-is.
                    //
                    // write_mask = COLOR (RGB, NOT alpha): a fully-covered glyph
                    // returns src alpha 1.0, and the premultiplied alpha equation
                    // (a = src_a + dst_a*(1-src_a)) would raise the SURFACE alpha
                    // from `opacity` to 1.0 for every text pixel — making a window
                    // full of text effectively opaque to the compositor regardless
                    // of the configured opacity. Masking out the alpha write keeps
                    // the surface alpha at `opacity` (set by the bg clear/quads), so
                    // the whole window — text included — composites at the chosen
                    // transparency, matching ghostty et al.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::COLOR,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: pipeline_cache.as_ref(),
        });

        // Overlay pipeline: same vertex/fragment as bg, but with premultiplied
        // alpha blending so translucent panel quads (modals, menus) composite over
        // the terminal pixels already on the surface instead of overwriting them.
        // `bg_instance_layout` was moved into `bg_pipeline`, so rebuild a fresh bg
        // instance layout here (identical attrs); `quad_layout` is still in scope.
        let overlay_instance_attrs = wgpu::vertex_attr_array![
            1 => Float32x2, // pos
            2 => Float32x2, // size
            3 => Float32x4, // color
        ];
        let overlay_instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<BgInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &overlay_instance_attrs,
        };
        // `quad_layout` was moved into `fg_pipeline`; rebuild a fresh unit-quad
        // vertex layout (identical) for the overlay pipeline's slot 0.
        let overlay_quad_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2],
        };
        let overlay_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay-pipeline"),
            layout: Some(&bg_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_bg"),
                buffers: &[overlay_quad_layout, overlay_instance_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_bg"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    // write_mask = ALL (RGB + alpha) — DELIBERATELY different from the
                    // fg pipeline. The chrome strip, tabs and modals are designed to
                    // RAISE the surface alpha so they read as near-opaque "glass"
                    // floating above the more-transparent terminal body (bar/surface
                    // alphas ≈ 0.92–0.97, the active tab is fully opaque). Masking the
                    // alpha here (as the fg pipeline does) would drop the whole chrome
                    // to the body opacity and make the toolbar/tabs/modals see-through
                    // to the desktop — so the alpha write is kept on purpose.
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: pipeline_cache.as_ref(),
        });

        log::info!("  renderer: pipelines/shaders ready {:.1} ms", ms(t));

        // --- Instance buffers, created with a small nonzero capacity. ---
        let bg_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bg-instances"),
            size: (INITIAL_INSTANCES * std::mem::size_of::<BgInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let fg_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fg-instances"),
            size: (INITIAL_INSTANCES * std::mem::size_of::<FgInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let image_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image-instances"),
            size: (64 * std::mem::size_of::<FgInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let overlay_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("overlay-instances"),
            size: (64 * std::mem::size_of::<BgInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let overlay_text_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("overlay-text-instances"),
            size: (64 * std::mem::size_of::<FgInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut renderer = Renderer {
            _window: window,
            surface,
            device,
            queue,
            config,
            bg_pipeline,
            fg_pipeline,
            overlay_pipeline,
            pipeline_cache,
            adapter_info,
            unit_quad,
            uniform_buffer,
            uniform_bind_group,
            mask_atlas_texture,
            color_atlas_texture,
            atlas_bind_group,
            // Image atlas is lazily allocated on the first draw_image call:
            image_atlas_texture: None,
            image_bind_group: None,
            image_atlas_bind_group_layout: atlas_bind_group_layout,
            image_atlas_sampler: atlas_sampler,
            image_packer: Packer::new(IMAGE_ATLAS_SIZE),
            image_cache: HashMap::new(),
            image_overlay: Vec::new(),
            image_buffer,
            image_capacity: 64,
            image_count: 0,
            overlay_quads: Vec::new(),
            overlay_buffer,
            overlay_capacity: 64,
            overlay_count: 0,
            overlay_text: Vec::new(),
            overlay_text_buffer,
            overlay_text_capacity: 64,
            overlay_text_count: 0,
            tab_overlay_quads: Vec::new(),
            tab_overlay_text: Vec::new(),
            tab_overlay_mark: None,
            packer: Packer::new(ATLAS_SIZE),
            color_packer: Packer::new(COLOR_ATLAS_SIZE),
            atlas_reset: false,
            glyph_cache: HashMap::new(),
            cluster_cache: HashMap::new(),
            ligature_run_cache: HashMap::new(),
            wide_char_set: std::collections::HashSet::new(),
            font_has_ligatures: false, // probed below after renderer is built
            ligatures_enabled: false,  // updated via set_ligatures()
            text,
            metrics,
            pad: pad_for(metrics.height),
            pad_top: None,
            pad_bottom: None,
            // Symmetric padding on all sides (ghostty-style minimal). Per-side
            // config overrides (set_pad_left etc.) still apply when present.
            pad_left: None,
            pad_right: None,
            grid_origin_y: 0.0,
            pad_override: None,
            font_px,
            font_family,
            font_features,
            rows: Vec::new(),
            cur_row: 0,
            bg_row_offsets: Vec::new(),
            fg_row_offsets: Vec::new(),
            bg_scratch_offsets: Vec::new(),
            fg_scratch_offsets: Vec::new(),
            dirty_rows: Vec::new(),
            bg_flat: Vec::with_capacity(INITIAL_INSTANCES),
            fg_flat: Vec::with_capacity(INITIAL_INSTANCES),
            bg_count: 0,
            fg_count: 0,
            bg_buffer,
            fg_buffer,
            bg_capacity: INITIAL_INSTANCES,
            fg_capacity: INITIAL_INSTANCES,
            clear_color: [0.0, 0.0, 0.0, 1.0],
            flash: None,
            opacity: opacity.clamp(0.0, 1.0),
            transparent,
            premultiplied_surface,
            mp: MultiPane::default(),
            // gpu-fx: retain the shader module + uniform layout so the CRT post
            // pipeline can be built lazily (only when enabled). Both are clones
            // of Arc-backed handles, so this is cheap and the grid pipelines
            // already hold their own references.
            crt_shader: shader,
            uniform_bind_group_layout,
            crt: crt::CrtPass::default(),
            cursor_trail: cursor_trail::CursorTrail::default(),
        };

        // Probe the loaded font for OpenType GSUB liga support. This shapes the
        // canonical "fi" test string: a liga font collapses it into one glyph
        // while a non-liga font keeps two. We log the result so operators can
        // confirm that the `ligatures` config key actually does something for their
        // chosen font.
        renderer.font_has_ligatures = renderer.text.has_ligatures();
        log::info!(
            "  renderer: font_has_ligatures={}",
            renderer.font_has_ligatures
        );

        log::info!("  renderer: ready {:.1} ms (total Renderer::new)", ms(t));

        Ok(renderer)
    }

    /// Pre-warm the glyph atlas with printable ASCII (regular + bold).
    ///
    /// Called on the tick immediately after the first frame is presented so the
    /// window appears ~10 ms sooner. Bold is included because shell prompts and
    /// editors render bold text on the first keypress; without it the first bold
    /// glyph triggers a visible atlas-upload stall.
    pub fn prewarm_ascii(&mut self) {
        let t = std::time::Instant::now();
        for byte in 0x20u8..=0x7E {
            self.ensure_glyphs(byte as char, false, false);
            self.ensure_glyphs(byte as char, true, false);
        }
        log::info!(
            "  renderer: ascii prewarm done {:.1} ms (deferred post-first-frame)",
            t.elapsed().as_secs_f64() * 1000.0
        );
    }
}
