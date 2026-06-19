// Two pipelines share group(0): a uniform carrying the surface size in physical
// pixels (vec4 so the buffer is 16-byte aligned; only .xy is used).
struct U { screen: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;

// --- Background pipeline: one solid-color quad per cell. --------------------
struct BgIn {
    @location(0) unit: vec2<f32>,   // unit quad corner in {0,1}^2 (slot 0)
    @location(1) pos: vec2<f32>,    // cell top-left in px (slot 1, instance)
    @location(2) size: vec2<f32>,   // cell size in px
    @location(3) color: vec4<f32>,
};
struct BgOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};
@vertex fn vs_bg(in: BgIn) -> BgOut {
    let px = in.pos + in.unit * in.size;
    let ndc = vec2<f32>(px.x / u.screen.x * 2.0 - 1.0, 1.0 - px.y / u.screen.y * 2.0);
    var o: BgOut;
    o.clip = vec4<f32>(ndc, 0.0, 1.0);
    o.color = in.color;
    return o;
}
@fragment fn fs_bg(in: BgOut) -> @location(0) vec4<f32> {
    return in.color;
}

// --- Foreground pipeline: one textured quad per glyph. ----------------------
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_samp: sampler;
struct FgIn {
    @location(0) unit: vec2<f32>,   // unit quad corner (slot 0)
    @location(1) pos: vec2<f32>,    // glyph quad top-left in px (slot 1, instance)
    @location(2) size: vec2<f32>,   // glyph quad size in px
    @location(3) uv_min: vec2<f32>, // atlas uv rect (0..1)
    @location(4) uv_max: vec2<f32>,
    @location(5) color: vec4<f32>,  // text/decoration color
    @location(6) flags: u32,        // 0 = coverage mask, 1 = color glyph, 2 = undercurl
};
struct FgOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) flags: u32,
    // For the undercurl path: the local quad size in px (for x-period + stroke
    // width) carried via uv_min/uv_max repurposed below; see vs_fg.
    @location(3) @interpolate(flat) quad_px: vec2<f32>,
};
@vertex fn vs_fg(in: FgIn) -> FgOut {
    let px = in.pos + in.unit * in.size;
    let ndc = vec2<f32>(px.x / u.screen.x * 2.0 - 1.0, 1.0 - px.y / u.screen.y * 2.0);
    var o: FgOut;
    o.clip = vec4<f32>(ndc, 0.0, 1.0);
    o.uv = mix(in.uv_min, in.uv_max, in.unit);
    o.color = in.color;
    o.flags = in.flags;
    o.quad_px = in.size;
    return o;
}

// Antialiased coverage of a curly (sine-wave) underline inside its decoration
// quad. The quad spans one cell width and a few pixels tall; the wave amplitude
// fills the quad with a margin for the stroke half-thickness so it never clips.
fn undercurl_coverage(uv: vec2<f32>, quad_px: vec2<f32>) -> f32 {
    let w = max(quad_px.x, 1.0);
    let h = max(quad_px.y, 1.0);
    // Stroke half-thickness in px (thinner relative to a thick quad reads best).
    let half = max(h * 0.18, 0.75);
    // One full sine period roughly every ~h*2 px gives a pleasant curl density;
    // clamp so very wide cells still get at least one visible wave.
    let period = clamp(h * 2.0, 6.0, w);
    let x = uv.x * w;
    let y = uv.y * h;
    let cy = h * 0.5;
    let amp = (h * 0.5) - half;
    let wave = cy + sin(x / period * 6.2831853) * amp;
    // Distance from this fragment to the wave centerline, softened to ~1px for AA.
    let d = abs(y - wave);
    return 1.0 - smoothstep(half - 0.75, half + 0.75, d);
}

// The foreground pass uses premultiplied-alpha blending so glyphs composite
// correctly over a translucent backdrop, so every branch below returns a
// premultiplied color (rgb already scaled by the output alpha).
@fragment fn fs_fg(in: FgOut) -> @location(0) vec4<f32> {
    if (in.flags == 2u) {
        // Undercurl: procedural sine-wave coverage tinted with the decoration color.
        let cov = undercurl_coverage(in.uv, in.quad_px);
        let a = in.color.a * cov;
        return vec4<f32>(in.color.rgb * a, a);
    }
    let texel = textureSample(atlas_tex, atlas_samp, in.uv);
    if (in.flags == 1u) {
        // Color glyph: the atlas holds straight-alpha RGBA, so premultiply it here
        // for the premultiplied-alpha blend (otherwise the edges fringe dark).
        let a = texel.a;
        return vec4<f32>(texel.rgb * a, a);
    }
    // Coverage mask: tint with the cell foreground, alpha = sampled coverage.
    let a = in.color.a * texel.a;
    return vec4<f32>(in.color.rgb * a, a);
}
