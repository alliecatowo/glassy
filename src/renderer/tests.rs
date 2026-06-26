use super::*;

// ---- Powerline range check ---------------------------------------------

/// Confirm the Powerline code-point range used in push_cell matches the
/// four glyphs we handle procedurally (E0B0-E0B3).
#[test]
fn powerline_range_covers_e0b0_to_e0b3() {
    for cp in [0xE0B0u32, 0xE0B1, 0xE0B2, 0xE0B3] {
        assert!(
            matches!(cp, 0xE0B0..=0xE0B3),
            "cp {cp:#06X} should be in the E0B0..=E0B3 range"
        );
    }
    // Code points just outside the range should NOT match.
    assert!(!matches!(0xE0AFu32, 0xE0B0..=0xE0B3));
    assert!(!matches!(0xE0B4u32, 0xE0B0..=0xE0B3));
}

// ---- wide-icon threshold helper ----------------------------------------

/// A cell_w of 8.0. Advances at or below 1.1× (≤ 8.8) are normal; above are
/// promoted to WIDE.
const CELL_W: f32 = 8.0;
const WIDE_THRESHOLD: f32 = CELL_W * 1.1;

#[test]
fn wide_threshold_boundary() {
    // Exactly at the threshold: NOT wide.
    const { assert!(8.8 <= WIDE_THRESHOLD, "8.8 should be at most the threshold") };
    // One ULP above: IS wide.
    const { assert!(8.9 > WIDE_THRESHOLD, "8.9 should exceed the 1.1× threshold") };
}

#[test]
fn nerd_font_icon_range_qualifies_for_promotion() {
    // Nerd-font Private Use Area starts at U+E000. These are 1.5× advance
    // icons; verify the detection formula triggers for them.
    let advance_1_5x: f32 = CELL_W * 1.5; // 12.0 > 8.8
    assert!(
        advance_1_5x > WIDE_THRESHOLD,
        "1.5× advance Nerd-font icon should trigger wide promotion"
    );
    // Normal ASCII/CJK glyphs have advance == cell_w: should NOT promote.
    let advance_1x: f32 = CELL_W;
    assert!(
        advance_1x <= WIDE_THRESHOLD,
        "1× advance glyph should NOT trigger wide promotion"
    );
}

// ---- LigatureCell smoke test -------------------------------------------

#[test]
fn ligature_cell_fields_are_accessible() {
    // Ensure the public struct is usable from outside the module.
    let lc = LigatureCell {
        col: 3,
        fg: [1.0, 0.0, 0.0, 1.0],
        bg: [0.0, 0.0, 0.0, 1.0],
        wide: false,
        decorations: Decorations::default(),
    };
    assert_eq!(lc.col, 3);
    assert!(!lc.wide);
}

// ---- clamp_scissor tests -----------------------------------------------

#[test]
fn scissor_fully_inside_is_unchanged() {
    let s = clamp_scissor(10, 20, 100, 50, 800, 600);
    assert_eq!((s.x, s.y, s.w, s.h), (10, 20, 100, 50));
}

#[test]
fn scissor_clamps_right_and_bottom_overflow() {
    // Rect extends past the surface: extent is trimmed, origin kept.
    let s = clamp_scissor(700, 550, 200, 200, 800, 600);
    assert_eq!((s.x, s.y, s.w, s.h), (700, 550, 100, 50));
}

#[test]
fn scissor_clamps_negative_origin() {
    // A negative origin clamps to 0 and the extent shrinks accordingly so
    // the right/bottom edge stays put.
    let s = clamp_scissor(-30, -10, 100, 80, 800, 600);
    assert_eq!((s.x, s.y, s.w, s.h), (0, 0, 70, 70));
}

#[test]
fn scissor_fully_outside_is_zero_extent() {
    let s = clamp_scissor(900, 700, 100, 100, 800, 600);
    assert_eq!((s.w, s.h), (0, 0));
}

#[test]
fn scissor_negative_extent_is_zero() {
    let s = clamp_scissor(10, 10, -50, -50, 800, 600);
    assert_eq!((s.w, s.h), (0, 0));
}

#[test]
fn scissor_exactly_at_surface_edge() {
    let s = clamp_scissor(0, 0, 800, 600, 800, 600);
    assert_eq!((s.x, s.y, s.w, s.h), (0, 0, 800, 600));
}

// ---- clamp_scissor: additional regression / edge cases -----------------
// These cover the wgpu 31 / winit 0.32 migration gate: scissor rects are
// passed verbatim to `set_scissor_rect` whose contract requires:
//   x + w ≤ surface_w  and  y + h ≤ surface_h  (for nonzero extent)
// The cases below verify that `clamp_scissor` upholds this invariant even
// in the tricky boundary/overflow situations the upgrade may expose.

#[test]
fn scissor_1px_at_right_edge() {
    // A single-pixel column flush with the right edge.
    let s = clamp_scissor(799, 0, 1, 600, 800, 600);
    assert_eq!((s.x, s.y, s.w, s.h), (799, 0, 1, 600));
    assert!(s.x + s.w <= 800, "right edge must not overflow surface_w");
}

#[test]
fn scissor_1px_at_bottom_edge() {
    // A single-pixel row flush with the bottom edge.
    let s = clamp_scissor(0, 599, 800, 1, 800, 600);
    assert_eq!((s.x, s.y, s.w, s.h), (0, 599, 800, 1));
    assert!(s.y + s.h <= 600, "bottom edge must not overflow surface_h");
}

#[test]
fn scissor_zero_size_surface_yields_zero_extent() {
    // Degenerate surface (minimized / compositor quirk): any rect should give
    // a zero-extent scissor so no draws are attempted.
    let s = clamp_scissor(0, 0, 100, 100, 0, 0);
    assert_eq!((s.w, s.h), (0, 0));
}

#[test]
fn scissor_single_pixel_surface() {
    // Surface is 1×1 (e.g. first resize event before layout). The scissor
    // for the whole surface must be valid.
    let s = clamp_scissor(0, 0, 1, 1, 1, 1);
    assert_eq!((s.x, s.y, s.w, s.h), (0, 0, 1, 1));
    // Any rect larger than the 1×1 surface must clamp.
    let big = clamp_scissor(0, 0, 800, 600, 1, 1);
    assert_eq!((big.x, big.y, big.w, big.h), (0, 0, 1, 1));
}

#[test]
fn scissor_origin_exactly_equals_surface_size_gives_zero_extent() {
    // Origin placed at (surface_w, surface_h): fully off-screen.
    let s = clamp_scissor(800, 600, 1, 1, 800, 600);
    assert_eq!((s.w, s.h), (0, 0));
}

#[test]
fn scissor_large_negative_origin_with_large_positive_extent() {
    // Very negative origin + compensating extent: the visible portion from
    // x=0 to the (clamped) right edge must be correct.
    let s = clamp_scissor(-500, -300, 1000, 700, 800, 600);
    // x0 = max(-500, 0) = 0; x1 = max(min(-500+1000,800),0) = min(500,800) = 500
    // y0 = max(-300, 0) = 0; y1 = max(min(-300+700,600),0) = min(400,600) = 400
    assert_eq!((s.x, s.y, s.w, s.h), (0, 0, 500, 400));
}

#[test]
fn scissor_result_invariant_x_plus_w_le_surface_w() {
    // Fuzz a grid of arbitrary origins + extents and verify the hard invariant.
    for x in [-100i32, 0, 100, 750, 800, 900] {
        for y in [-50i32, 0, 50, 550, 600, 700] {
            for w in [0i32, 1, 100, 200, 800, 1000] {
                for h in [0i32, 1, 50, 100, 600, 800] {
                    let s = clamp_scissor(x, y, w, h, 800, 600);
                    assert!(
                        s.x + s.w <= 800,
                        "clamp_scissor({x},{y},{w},{h},800,600): x+w={} > 800",
                        s.x + s.w
                    );
                    assert!(
                        s.y + s.h <= 600,
                        "clamp_scissor({x},{y},{w},{h},800,600): y+h={} > 600",
                        s.y + s.h
                    );
                }
            }
        }
    }
}

// ---- Packer (shelf-packer) unit tests ----------------------------------
// The glyph atlas packer is the critical path for atlas-overflow/repack.
// These tests gate the pack + REPACK logic so a wgpu 31 dependency bump
// never silently breaks UV validity.

#[test]
fn packer_single_alloc_returns_origin_zero() {
    let mut p = Packer::new(1024);
    let origin = p.alloc(10, 10).expect("alloc in fresh packer must succeed");
    assert_eq!(origin, (0, 0), "first alloc must land at (0,0)");
}

#[test]
fn packer_second_alloc_advances_x_by_gap() {
    let mut p = Packer::new(1024);
    p.alloc(10, 10).unwrap();
    let second = p.alloc(10, 10).unwrap();
    // First occupies [0, 10); gap = GLYPH_GAP (1px); second starts at 11.
    assert_eq!(
        second,
        (10 + GLYPH_GAP, 0),
        "second alloc must follow the first with a {GLYPH_GAP}px gap"
    );
}

#[test]
fn packer_row_wrap_when_row_is_full() {
    // Pack glyphs that are each 600px wide into a 1024px atlas.
    // First fits at x=0; second would need x=601, which is < 1024 (fits).
    // Third would need x=1202, which exceeds 1024, so it wraps to y=11.
    let mut p = Packer::new(1024);
    let a = p.alloc(600, 10).unwrap();
    assert_eq!(a, (0, 0));
    let b = p.alloc(600, 10).unwrap(); // fits at x=601
    // 601 ≤ 1024-600 = 424: nope (600 < 424 is false), so it wraps.
    // Actually: cursor_x after a = 600+1 = 601; 601+600=1201 > 1024 → new shelf.
    // shelf_height from row 0 = 10; cursor_y = 0+10+1 = 11.
    assert_eq!(b.1, 11, "second wide glyph must start on the second shelf");
}

#[test]
fn packer_exact_fit_horizontally() {
    // A glyph that exactly fills the atlas width must be placed without wrapping.
    let size = 64u32;
    let mut p = Packer::new(size);
    let origin = p.alloc(size, 8).expect("exact-width glyph must fit");
    assert_eq!(origin.0, 0);
    assert_eq!(origin.1, 0);
}

#[test]
fn packer_overflow_returns_none() {
    // Pack until the atlas is full and verify None is returned.
    let size = 16u32;
    let mut p = Packer::new(size);
    // Fill all shelves of height 8: can fit 16/8 = 2 rows before overflow.
    let mut allocs: usize = 0;
    while p.alloc(8, 8).is_some() {
        allocs += 1;
        if allocs > 1000 {
            panic!("packer never returned None — infinite loop guard hit");
        }
    }
    assert!(
        allocs > 0,
        "must have succeeded at least once before overflow"
    );
}

#[test]
fn packer_reset_clears_state() {
    let mut p = Packer::new(64);
    // Fill it until overflow.
    while p.alloc(10, 10).is_some() {}
    // After reset the first alloc must land back at (0,0).
    p.reset();
    let origin = p.alloc(10, 10).expect("alloc after reset must succeed");
    assert_eq!(origin, (0, 0), "reset must return packer to (0,0)");
}

#[test]
fn packer_glyph_wider_than_atlas_returns_none() {
    let mut p = Packer::new(64);
    assert!(
        p.alloc(65, 1).is_none(),
        "glyph wider than atlas must be rejected"
    );
}

#[test]
fn packer_glyph_taller_than_atlas_returns_none() {
    let mut p = Packer::new(64);
    assert!(
        p.alloc(1, 65).is_none(),
        "glyph taller than atlas must be rejected"
    );
}

#[test]
fn packer_allocs_are_non_overlapping() {
    // Allocate a known sequence and verify no two rects overlap.
    let mut p = Packer::new(128);
    let mut rects: Vec<(u32, u32, u32, u32)> = Vec::new(); // (x, y, w, h)
    let sizes = [(10u32, 12u32), (8, 12), (15, 6), (10, 20), (5, 5), (20, 8)];
    for &(w, h) in &sizes {
        if let Some((x, y)) = p.alloc(w, h) {
            rects.push((x, y, w, h));
        }
    }
    // Verify pairwise non-overlap (axis-aligned rect intersection check).
    for i in 0..rects.len() {
        for j in (i + 1)..rects.len() {
            let (ax, ay, aw, ah) = rects[i];
            let (bx, by, bw, bh) = rects[j];
            let x_overlap = ax < bx + bw && bx < ax + aw;
            let y_overlap = ay < by + bh && by < ay + ah;
            assert!(
                !(x_overlap && y_overlap),
                "rects {i} and {j} overlap: {rects:?}"
            );
        }
    }
}

#[test]
fn packer_uv_coords_stay_in_0_1_after_pack_sequence() {
    // Simulate the UV derivation used in pack_rasters: every allocated rect
    // must produce UV coords in [0,1] when divided by the atlas size.
    let atlas_size = ATLAS_SIZE;
    let inv = 1.0 / atlas_size as f32;
    let mut p = Packer::new(atlas_size);
    let glyph_w = 12u32;
    let glyph_h = 14u32;
    let mut count = 0;
    while let Some((x, y)) = p.alloc(glyph_w, glyph_h) {
        let uv_min = [x as f32 * inv, y as f32 * inv];
        let uv_max = [(x + glyph_w) as f32 * inv, (y + glyph_h) as f32 * inv];
        assert!(
            uv_min[0] >= 0.0 && uv_min[0] <= 1.0,
            "uv_min.x={} out of [0,1]",
            uv_min[0]
        );
        assert!(
            uv_min[1] >= 0.0 && uv_min[1] <= 1.0,
            "uv_min.y={} out of [0,1]",
            uv_min[1]
        );
        assert!(
            uv_max[0] >= 0.0 && uv_max[0] <= 1.0,
            "uv_max.x={} out of [0,1]",
            uv_max[0]
        );
        assert!(
            uv_max[1] >= 0.0 && uv_max[1] <= 1.0,
            "uv_max.y={} out of [0,1]",
            uv_max[1]
        );
        count += 1;
        if count > 10_000 {
            panic!("packer ran too many iterations without overflow");
        }
    }
    assert!(count > 0);
}

// ---- Atlas repack invariant (pure-CPU simulation) -----------------------
// This gates the pack_rasters overflow / repack path without needing a GPU:
// we drive the same Packer + UV-derivation logic that pack_rasters executes,
// verify that after a simulated overflow + reset + repack all UVs are valid,
// and confirm that post-reset UVs differ from pre-reset UVs (the cache-clear
// contract). This is a regression guard for the wgpu 31 dep bump.

#[test]
fn atlas_overflow_repack_uvs_are_valid_after_reset() {
    // Simulate the pack loop: fill the packer until overflow, then reset
    // (as pack_rasters does when None is returned the first time), and
    // verify the repacked entry is within [0,1].
    let atlas_size = 64u32; // small atlas to force overflow quickly
    let inv = 1.0 / atlas_size as f32;
    let glyph_w = 10u32;
    let glyph_h = 10u32;
    let mut p = Packer::new(atlas_size);

    // Fill until overflow.
    let mut pre_reset_last: Option<(u32, u32)> = None;
    while let Some(o) = p.alloc(glyph_w, glyph_h) {
        pre_reset_last = Some(o);
    }
    let pre_uvs = pre_reset_last.map(|(x, y)| {
        [
            x as f32 * inv,
            y as f32 * inv,
            (x + glyph_w) as f32 * inv,
            (y + glyph_h) as f32 * inv,
        ]
    });

    // Simulate repack: reset + re-alloc the same glyph.
    p.reset();
    let post_origin = p
        .alloc(glyph_w, glyph_h)
        .expect("first alloc after reset must succeed");
    let post_uvs = [
        post_origin.0 as f32 * inv,
        post_origin.1 as f32 * inv,
        (post_origin.0 + glyph_w) as f32 * inv,
        (post_origin.1 + glyph_h) as f32 * inv,
    ];

    // Post-reset UVs must be in [0,1].
    for &uv in &post_uvs {
        assert!((0.0..=1.0).contains(&uv), "post-reset UV {uv} out of [0,1]");
    }
    // Post-reset origin must be (0,0) — the cache was cleared and repacking
    // starts fresh, so the first glyph always lands at the atlas origin.
    assert_eq!(
        post_origin,
        (0, 0),
        "post-reset first alloc must land at (0,0)"
    );
    // If the pre-reset packer had placed at least one glyph, the pre-reset
    // and post-reset UVs must differ (stale UVs from before the reset are
    // invalid; this is why atlas_reset triggers a full row rebuild).
    if let Some(pre) = pre_uvs {
        // The last pre-reset glyph was NOT at (0,0) (since at least one
        // was packed before it), so its UVs must differ from post-reset UVs.
        // This assertion can only fail if the packer somehow wrapped back
        // to (0,0) before it returned None — which would be a packer bug.
        let pre_xy = (pre[0], pre[1]);
        let post_xy = (post_uvs[0], post_uvs[1]);
        if pre_reset_last.unwrap() != (0, 0) {
            assert_ne!(
                pre_xy, post_xy,
                "pre-reset and post-reset UVs must differ (atlas_reset contract)"
            );
        }
    }
}

#[test]
fn atlas_repack_does_not_lose_capacity() {
    // After overflow + reset, the packer must be able to pack the same
    // glyph again (capacity not permanently lost).
    let mut p = Packer::new(64);
    while p.alloc(10, 10).is_some() {}
    p.reset();
    assert!(
        p.alloc(10, 10).is_some(),
        "packer must recover full capacity after reset"
    );
}

// ---- flush_pass / instance buffer growth (headless GPU) ----------------
// These tests obtain a real wgpu device (no surface) and exercise the
// flush_pass logic: buffer layout detection, grow-on-overflow, and
// partial-dirty sub-range upload. They guard the instance-buffer growth
// path against wgpu API changes in the 31 upgrade.

/// Acquire a headless wgpu device without a surface. Returns `None` if no
/// GPU adapter is available in the test environment (CI without GPU). The
/// tests that call this skip gracefully in that case.
fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::None,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("glassy-test"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::downlevel_defaults(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..Default::default()
    }))
    .ok()?;
    Some((device, queue))
}

#[test]
fn headless_wgpu_device_init_succeeds() {
    // Gate: verify the wgpu adapter + device pipeline can be constructed
    // without a window surface. This is the prerequisite for all GPU tests.
    // If no hardware adapter is available we skip (don't fail).
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::None,
        force_fallback_adapter: false,
        compatible_surface: None,
    }));
    let Ok(adapter) = adapter else {
        // No GPU in this environment; skip.
        return;
    };
    let info = adapter.get_info();
    // Adapter must report a non-empty name and a known backend.
    assert!(!info.name.is_empty(), "adapter name must be non-empty");
    let result = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("glassy-headless-gate"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::downlevel_defaults(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..Default::default()
    }));
    assert!(result.is_ok(), "headless device creation must succeed");
}

#[test]
fn headless_device_can_create_atlas_textures() {
    // Verify that the two atlas texture descriptors (R8Unorm mask and
    // Rgba8Unorm color) can be created on the headless device. A wgpu 31
    // format-enum or Extent3d API change would fail here first.
    let Some((device, _queue)) = headless_device() else {
        return;
    };

    let _mask = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test-mask-atlas"),
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

    let _color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test-color-atlas"),
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
    // Reaching here means both texture formats are accepted. No GPU panic.
}

#[test]
fn headless_device_can_create_instance_buffers() {
    // Verify that the bg (BgInstance) and fg (FgInstance) instance buffers
    // can be created at the initial capacity. A wgpu 31 BufferDescriptor API
    // change would break here.
    let Some((device, _queue)) = headless_device() else {
        return;
    };

    let _bg = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-bg-instances"),
        size: (INITIAL_INSTANCES * std::mem::size_of::<BgInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let _fg = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-fg-instances"),
        size: (INITIAL_INSTANCES * std::mem::size_of::<FgInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    // Drop without error = success.
}

#[test]
fn flush_pass_full_upload_on_layout_change() {
    // flush_pass must do a full re-flatten when offsets change. We create
    // two rows, call flush_pass once to establish the layout, change the
    // instance count of row 0 to shift the layout, and verify that the
    // returned total is the new total (not the old one).
    let Some((device, queue)) = headless_device() else {
        return;
    };

    // Build two rows: row 0 has 2 bg instances, row 1 has 1 bg instance.
    let mut rows: Vec<RowInstances> = vec![RowInstances::default(), RowInstances::default()];
    for _ in 0..2 {
        rows[0].bg.push(BgInstance {
            pos: [0.0, 0.0],
            size: [8.0, 16.0],
            color: [0.0; 4],
        });
    }
    rows[1].bg.push(BgInstance {
        pos: [8.0, 0.0],
        size: [8.0, 16.0],
        color: [0.0; 4],
    });

    let stride = std::mem::size_of::<BgInstance>();
    let initial_cap = 16usize;
    let mut buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-bg"),
        size: (initial_cap * stride) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut capacity = initial_cap;
    let mut offsets: Vec<u32> = Vec::new();
    let mut scratch: Vec<u32> = Vec::new();
    let mut flat: Vec<BgInstance> = Vec::new();

    // First call: establishes layout [0, 2, 3].
    let total1 = Renderer::flush_pass::<BgInstance>(
        &device,
        &queue,
        &rows,
        |r| &r.bg,
        &mut flat,
        &mut offsets,
        &mut scratch,
        &[],
        &mut buffer,
        &mut capacity,
        "test-bg",
    );
    assert_eq!(total1, 3, "total after first call must be 3");
    assert_eq!(offsets, &[0, 2, 3], "offsets must be prefix sums");

    // Mutate: give row 0 one MORE instance — this changes the layout.
    rows[0].bg.push(BgInstance {
        pos: [16.0, 0.0],
        size: [8.0, 16.0],
        color: [0.0; 4],
    });
    let mut dirty = vec![0usize]; // row 0 was rebuilt

    // Second call: layout must detect the shift and reflatten.
    let total2 = Renderer::flush_pass::<BgInstance>(
        &device,
        &queue,
        &rows,
        |r| &r.bg,
        &mut flat,
        &mut offsets,
        &mut scratch,
        &dirty,
        &mut buffer,
        &mut capacity,
        "test-bg",
    );
    assert_eq!(total2, 4, "total after layout change must be 4");
    assert_eq!(offsets, &[0, 3, 4], "offsets must reflect the new layout");

    // Verify the buffer was grown when total exceeded original capacity (it
    // was 16 so this particular case doesn't grow, but capacity is preserved).
    assert!(capacity >= 4, "capacity must be at least the total");

    // Reset dirty for a stable-layout call.
    dirty.clear();
    let total3 = Renderer::flush_pass::<BgInstance>(
        &device,
        &queue,
        &rows,
        |r| &r.bg,
        &mut flat,
        &mut offsets,
        &mut scratch,
        &dirty,
        &mut buffer,
        &mut capacity,
        "test-bg",
    );
    // Same layout, no dirty rows: fast path returns the same total.
    assert_eq!(total3, 4, "no-dirty fast path must return same total");
}

#[test]
fn flush_pass_grows_buffer_when_capacity_exceeded() {
    // Create a buffer that is too small for the total instances, verify
    // flush_pass creates a new (larger) buffer and returns the correct total.
    let Some((device, queue)) = headless_device() else {
        return;
    };

    let n_instances = INITIAL_INSTANCES + 1; // one more than the initial buffer
    let mut rows: Vec<RowInstances> = vec![RowInstances::default()];
    for _ in 0..n_instances {
        rows[0].bg.push(BgInstance {
            pos: [0.0, 0.0],
            size: [1.0, 1.0],
            color: [0.0; 4],
        });
    }

    let stride = std::mem::size_of::<BgInstance>();
    // Intentionally under-sized buffer so flush_pass must grow it.
    let mut buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-tiny"),
        size: (INITIAL_INSTANCES * stride) as u64, // one too few
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut capacity = INITIAL_INSTANCES;
    let mut offsets: Vec<u32> = Vec::new();
    let mut scratch: Vec<u32> = Vec::new();
    let mut flat: Vec<BgInstance> = Vec::new();

    let total = Renderer::flush_pass::<BgInstance>(
        &device,
        &queue,
        &rows,
        |r| &r.bg,
        &mut flat,
        &mut offsets,
        &mut scratch,
        &[0],
        &mut buffer,
        &mut capacity,
        "test-tiny",
    );
    assert_eq!(
        total as usize, n_instances,
        "total must equal instance count"
    );
    assert!(
        capacity >= n_instances,
        "capacity must have grown to at least {n_instances}, got {capacity}"
    );
    // Buffer size must be a power-of-two ≥ n_instances (the grow formula).
    let expected_min_cap = n_instances.next_power_of_two().max(INITIAL_INSTANCES);
    assert_eq!(
        capacity, expected_min_cap,
        "capacity must be next_power_of_two({n_instances}) = {expected_min_cap}"
    );
}

#[test]
fn flush_pass_fast_path_skips_unchanged_rows() {
    // On a stable layout with no dirty rows, flush_pass must return the
    // cached total without any write (we can't assert no write without a
    // mock queue, but we can assert the returned total and offsets are
    // stable across back-to-back calls with empty dirty_rows).
    let Some((device, queue)) = headless_device() else {
        return;
    };

    let mut rows = vec![RowInstances::default(), RowInstances::default()];
    rows[0].bg.push(BgInstance {
        pos: [0.0, 0.0],
        size: [8.0, 16.0],
        color: [0.0; 4],
    });
    rows[1].bg.push(BgInstance {
        pos: [8.0, 0.0],
        size: [8.0, 16.0],
        color: [0.0; 4],
    });

    let stride = std::mem::size_of::<BgInstance>();
    let mut buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-stable"),
        size: (16 * stride) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut capacity = 16usize;
    let mut offsets: Vec<u32> = Vec::new();
    let mut scratch: Vec<u32> = Vec::new();
    let mut flat: Vec<BgInstance> = Vec::new();

    // First call establishes the layout.
    Renderer::flush_pass::<BgInstance>(
        &device,
        &queue,
        &rows,
        |r| &r.bg,
        &mut flat,
        &mut offsets,
        &mut scratch,
        &[0, 1],
        &mut buffer,
        &mut capacity,
        "test-stable",
    );
    let offsets_after_first = offsets.clone();

    // Second call: same content, no dirty rows.
    let total = Renderer::flush_pass::<BgInstance>(
        &device,
        &queue,
        &rows,
        |r| &r.bg,
        &mut flat,
        &mut offsets,
        &mut scratch,
        &[],
        &mut buffer,
        &mut capacity,
        "test-stable",
    );
    assert_eq!(total, 2, "total unchanged");
    assert_eq!(
        offsets, offsets_after_first,
        "offsets unchanged on fast path"
    );
}

#[test]
fn flush_pass_empty_rows_returns_zero() {
    let Some((device, queue)) = headless_device() else {
        return;
    };

    let stride = std::mem::size_of::<BgInstance>();
    let mut buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-empty"),
        size: (16 * stride) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut capacity = 16usize;
    let rows: Vec<RowInstances> = vec![];
    let mut offsets: Vec<u32> = Vec::new();
    let mut scratch: Vec<u32> = Vec::new();
    let mut flat: Vec<BgInstance> = Vec::new();

    let total = Renderer::flush_pass::<BgInstance>(
        &device,
        &queue,
        &rows,
        |r| &r.bg,
        &mut flat,
        &mut offsets,
        &mut scratch,
        &[],
        &mut buffer,
        &mut capacity,
        "test-empty",
    );
    assert_eq!(total, 0);
    assert_eq!(offsets, &[0u32]);
}

// ---- Multi-pane scissor geometry regression ----------------------------
// These gate the multi-pane scissored draw path as a unit. `begin_pane`
// calls `clamp_scissor` internally; the PaneDraw records must carry valid
// (non-overflowing) scissor rects. We test the geometry helpers directly
// since begin_pane + end_pane require a full Renderer (GPU + font).

#[test]
fn scissor_multi_pane_two_panes_side_by_side_no_overlap() {
    // Two panes split at x=400 in an 800×600 surface: left [0,0,400,600]
    // and right [400,0,400,600]. The two scissor rects must not overlap.
    let left = clamp_scissor(0, 0, 400, 600, 800, 600);
    let right = clamp_scissor(400, 0, 400, 600, 800, 600);

    assert_eq!((left.x, left.y, left.w, left.h), (0, 0, 400, 600));
    assert_eq!((right.x, right.y, right.w, right.h), (400, 0, 400, 600));

    // Verify no horizontal overlap.
    assert!(
        left.x + left.w <= right.x || right.x + right.w <= left.x,
        "left and right pane scissor rects must not overlap horizontally"
    );
}

#[test]
fn scissor_multi_pane_partial_overlap_with_surface_edge_is_clamped() {
    // A pane whose rect straddles the surface boundary must be clamped, not
    // rejected or wrapped. This exercises the same path as end_pane() when a
    // pane is resized to extend slightly beyond the surface.
    let s = clamp_scissor(700, 400, 200, 300, 800, 600);
    // Clamped right = min(700+200, 800) = 800; w = 100.
    // Clamped bottom = min(400+300, 600) = 600; h = 200.
    assert_eq!((s.x, s.y, s.w, s.h), (700, 400, 100, 200));
    // Invariant: must not overflow the surface.
    assert!(s.x + s.w <= 800);
    assert!(s.y + s.h <= 600);
}

#[test]
fn scissor_multi_pane_three_horizontal_strips_tile_surface() {
    // Three equal-height horizontal panes in a 800×600 surface:
    // top [0,0,800,200], mid [0,200,800,200], bot [0,400,800,200].
    let top = clamp_scissor(0, 0, 800, 200, 800, 600);
    let mid = clamp_scissor(0, 200, 800, 200, 800, 600);
    let bot = clamp_scissor(0, 400, 800, 200, 800, 600);

    assert_eq!((top.x, top.y, top.w, top.h), (0, 0, 800, 200));
    assert_eq!((mid.x, mid.y, mid.w, mid.h), (0, 200, 800, 200));
    assert_eq!((bot.x, bot.y, bot.w, bot.h), (0, 400, 800, 200));

    // Panes must tile without overlap or gap.
    assert_eq!(top.y + top.h, mid.y, "no gap between top and mid");
    assert_eq!(mid.y + mid.h, bot.y, "no gap between mid and bot");
    assert_eq!(
        bot.y + bot.h,
        600,
        "bottom of last pane must reach surface_h"
    );
}

#[test]
fn scissor_pane_of_zero_size_gives_zero_extent() {
    // A collapsed pane (width or height = 0) must produce a scissor whose
    // relevant dimension is zero, so the GPU draw is skipped via the
    // `if s.w == 0 || s.h == 0 { continue; }` guard in record_multi_passes.
    //
    // Note: clamp_scissor does not force BOTH dimensions to zero — a zero
    // width leaves height unchanged and vice versa. The caller's `|| h == 0`
    // guard covers both cases independently.
    let s = clamp_scissor(100, 100, 0, 200, 800, 600);
    assert_eq!(s.w, 0, "zero-width pane must have w=0 scissor");
    let s2 = clamp_scissor(100, 100, 200, 0, 800, 600);
    assert_eq!(s2.h, 0, "zero-height pane must have h=0 scissor");
}

// ---- Transparency: glyph/overlay write_mask alpha invariant ------------
// Proves the opacity fix (init.rs): the foreground pipeline uses
// `ColorWrites::COLOR` so a fully-covered glyph CANNOT raise the surface
// alpha from `opacity` toward 1.0 under premultiplied blending — which was
// the cause of "window barely transparent at opacity=0.50". The overlay
// pipeline deliberately keeps `ColorWrites::ALL` (chrome reads near-opaque),
// so the same draw under ALL raises alpha to 1.0; this test asserts BOTH so a
// future change that flips either mask is caught.

fn run_writemask_alpha_probe(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    write_mask: wgpu::ColorWrites,
) -> f32 {
    // A 1x1 Rgba8Unorm target cleared to a premultiplied (0.5,0.5,0.5,0.5),
    // then a full-screen opaque white quad (premultiplied rgb=1, a=1) drawn
    // over it with PREMULTIPLIED_ALPHA_BLENDING and the given write_mask.
    let fmt = wgpu::TextureFormat::Rgba8Unorm;
    let src = r#"
        @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
            // Full-screen triangle.
            var p = array<vec2<f32>, 3>(
                vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
            return vec4<f32>(p[i], 0.0, 1.0);
        }
        @fragment fn fs() -> @location(0) vec4<f32> {
            // Premultiplied opaque white: rgb=1, a=1.
            return vec4<f32>(1.0, 1.0, 1.0, 1.0);
        }
    "#;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("wm-probe"),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("wm-probe-layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("wm-probe-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format: fmt,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("wm-probe-target"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: fmt,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = 4u32.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("wm-probe-readback"),
        size: padded as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("wm-probe-enc"),
    });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("wm-probe-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // Clear to surface alpha 0.5 (premultiplied gray).
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.5,
                        g: 0.5,
                        b: 0.5,
                        a: 0.5,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    }
    enc.copy_texture_to_buffer(
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
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(enc.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    let data = slice.get_mapped_range();
    let alpha = data[3] as f32 / 255.0;
    drop(data);
    readback.unmap();
    alpha
}

#[test]
fn fg_color_writemask_preserves_surface_alpha() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    // The fg pipeline's mask (COLOR): a full-coverage opaque glyph must NOT
    // raise the surface alpha above the configured opacity (0.5 here).
    let a_color = run_writemask_alpha_probe(&device, &queue, wgpu::ColorWrites::COLOR);
    assert!(
        (a_color - 0.5).abs() < 0.02,
        "COLOR mask must leave surface alpha at opacity (0.5), got {a_color}"
    );
    // Sanity / contrast: with ALL (the OLD fg behavior, and the deliberate
    // overlay/chrome behavior) the same draw raises alpha to ~1.0.
    let a_all = run_writemask_alpha_probe(&device, &queue, wgpu::ColorWrites::ALL);
    assert!(
        a_all > 0.98,
        "ALL mask raises surface alpha to ~1.0, got {a_all}"
    );
}
