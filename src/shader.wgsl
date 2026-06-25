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
// Two atlases: an R8 coverage-mask atlas for ordinary text (binding 0) and an
// RGBA8 color atlas for emoji (binding 2). They share one sampler (binding 1).
@group(1) @binding(0) var mask_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_samp: sampler;
@group(1) @binding(2) var color_tex: texture_2d<f32>;
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
    // For the rounded-rect path (flags==3): the corner radius in px, carried out
    // of band in uv_min.x so the interpolated `uv` stays a clean 0..1 local coord.
    @location(4) @interpolate(flat) radius_px: f32,
    // For the per-corner rounded-rect path (flags==4): the four corner radii in px
    // (top-left, top-right, bottom-right, bottom-left), carried out of band via
    // uv_min/uv_max so the interpolated `uv` stays a clean 0..1 local coord.
    @location(5) @interpolate(flat) radii4: vec4<f32>,
};
@vertex fn vs_fg(in: FgIn) -> FgOut {
    let px = in.pos + in.unit * in.size;
    let ndc = vec2<f32>(px.x / u.screen.x * 2.0 - 1.0, 1.0 - px.y / u.screen.y * 2.0);
    var o: FgOut;
    o.clip = vec4<f32>(ndc, 0.0, 1.0);
    // For flags==3/4 the atlas UVs are unused, so the caller smuggles radius data
    // through uv_min/uv_max; `unit` then gives the 0..1 local coord directly.
    let rrect = in.flags == 3u || in.flags == 4u;
    o.uv = select(mix(in.uv_min, in.uv_max, in.unit), in.unit, rrect);
    o.color = in.color;
    o.flags = in.flags;
    o.quad_px = in.size;
    o.radius_px = in.uv_min.x;
    // flags==4: per-corner radii packed as uv_min=(tl,tr), uv_max=(br,bl).
    o.radii4 = vec4<f32>(in.uv_min.x, in.uv_min.y, in.uv_max.x, in.uv_max.y);
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

// Signed distance to a rounded rectangle centered at the origin. `half` is the
// box half-extent, `r` the corner radius; negative inside, positive outside.
fn sdf_rrect(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2<f32>(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - r;
}

// Signed distance to a rounded rectangle with independent per-corner radii.
// `radii` is (top-left, top-right, bottom-right, bottom-left) — i.e. the order
// (-x,-y), (+x,-y), (+x,+y), (-x,+y) in this y-down clip space (top = -y). The
// radius for the quadrant `p` falls in is selected, then the standard rrect SDF
// is evaluated against that single radius.
fn sdf_rrect4(p: vec2<f32>, half: vec2<f32>, radii: vec4<f32>) -> f32 {
    // Select the radius for the quadrant this fragment falls in. Top row (p.y<0)
    // uses tl/tr; bottom row uses bl/br; left column (p.x<0) picks the *-left value.
    let r_top = select(radii.y, radii.x, p.x < 0.0); // tr vs tl
    let r_bot = select(radii.z, radii.w, p.x < 0.0); // br vs bl
    let rr = select(r_bot, r_top, p.y < 0.0);
    let q = abs(p) - half + vec2<f32>(rr, rr);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - rr;
}

// sRGB transfer functions (IEC 61966-2-1). The surface is a plain UNORM (non-
// sRGB) format, so values written to it are interpreted literally as gamma-
// encoded sRGB and the fixed-function blend composites in that gamma space. To
// apply glyph coverage in LINEAR light we decode the text color to linear here,
// weight it by coverage, then re-encode to sRGB before returning. This keeps the
// thin-stroke edge weighting perceptually correct (no over-thin / over-heavy
// fringes) without changing the hue of the (fully covered) interior pixels.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

// Apply `cov` glyph coverage to an sRGB foreground color in linear space and
// return a PREMULTIPLIED sRGB result for the premultiplied-alpha blend. The
// linear color is scaled by coverage (the physically correct partial-pixel
// weighting) and re-encoded; the returned alpha is the coverage itself so the
// destination is weighted by (1 - cov) as before. At cov == 1 this is an exact
// round-trip (interior hue unchanged); at the edges the coverage now darkens the
// stroke along the linear ramp, matching high-quality renderers.
fn coverage_blend(color: vec3<f32>, cov: f32) -> vec4<f32> {
    let lin = srgb_to_linear(color) * cov;
    return vec4<f32>(linear_to_srgb(lin), cov);
}

// The foreground pass uses premultiplied-alpha blending so glyphs composite
// correctly over a translucent backdrop, so every branch below returns a
// premultiplied color (rgb already scaled by the output alpha).
@fragment fn fs_fg(in: FgOut) -> @location(0) vec4<f32> {
    if (in.flags == 4u) {
        // Per-corner rounded-rect solid fill. Same AA treatment as flags==3 but
        // each corner gets its own radius (top-rounded tabs etc.). Radii are
        // clamped to the box half-extent so they degrade gracefully.
        let half = in.quad_px * 0.5;
        let p = in.uv * in.quad_px - half;
        let lim = min(half.x, half.y);
        let radii = min(in.radii4, vec4<f32>(lim, lim, lim, lim));
        let d = sdf_rrect4(p, half, radii);
        let fw = clamp(0.5 * fwidth(d), 0.25, 1.5);
        let cov = (1.0 - smoothstep(-fw, fw, d)) * in.color.a;
        return vec4<f32>(in.color.rgb * cov, cov);
    }
    if (in.flags == 3u) {
        // Rounded-rect solid fill: exact SDF from the flat-interpolated quad size
        // and the 0..1 local coord, fwidth-based AA band so corners are crisp at
        // all DPIs and scale factors. The radius is clamped to the box so it
        // degrades to a plain rect when 0 and a stadium when big.
        let half = in.quad_px * 0.5;
        let p = in.uv * in.quad_px - half;
        let r = min(in.radius_px, min(half.x, half.y));
        let d = sdf_rrect(p, half, r);
        // Use fwidth to derive the AA half-band from the actual screen-space
        // derivative of the SDF: 0.5*fwidth(d) is the exact pixel-radius of the
        // transition zone at any DPI / zoom. Clamp to [0.25, 1.5] so the edge
        // never collapses to a hard step on very small radii or spreads too
        // widely on low-DPI displays.
        let fw = clamp(0.5 * fwidth(d), 0.25, 1.5);
        let cov = (1.0 - smoothstep(-fw, fw, d)) * in.color.a;
        return vec4<f32>(in.color.rgb * cov, cov);
    }
    if (in.flags == 2u) {
        // Undercurl: procedural sine-wave coverage tinted with the decoration
        // color, blended in linear space like the coverage-mask glyph path.
        let cov = undercurl_coverage(in.uv, in.quad_px) * in.color.a;
        return coverage_blend(in.color.rgb, cov);
    }
    if (in.flags == 1u) {
        // Color glyph: the color atlas holds straight-alpha RGBA, so premultiply
        // it here for the premultiplied-alpha blend (otherwise the edges fringe
        // dark). Color emoji carry their own (already gamma-encoded) RGB, so we
        // leave them untouched and only premultiply — no linear re-tinting.
        let texel = textureSample(color_tex, atlas_samp, in.uv);
        let a = texel.a;
        return vec4<f32>(texel.rgb * a, a);
    }
    // Coverage mask: the R8 mask atlas carries coverage in the red channel. Tint
    // with the cell foreground, coverage applied in linear space for gamma-correct
    // antialiasing of the glyph edges.
    let cov = in.color.a * textureSample(mask_tex, atlas_samp, in.uv).r;
    return coverage_blend(in.color.rgb, cov);
}

// --- CRT / glow / scanline post-process pass. -------------------------------
// OPT-IN (config `crt_effect`, default off). When the effect is disabled the
// host never adds this pass at all (the offscreen target + composite are not
// even allocated), so the default build pays zero cost and keeps the 0%-idle /
// memory benchmarks intact. When enabled, the grid is first rendered to an
// offscreen RGBA texture, then this pass samples that texture full-screen and
// composites it to the surface with barrel curvature, scanlines, an aperture-
// grille tint, a cheap separable-ish glow (a few taps), and vignette.
//
// group(0): the shared screen-size uniform `u` (reused for resolution).
// group(2): the offscreen scene texture + sampler + the CRT parameter uniform.
@group(2) @binding(0) var crt_tex: texture_2d<f32>;
@group(2) @binding(1) var crt_samp: sampler;
struct CrtU {
    // x = curvature strength (0 = flat), y = scanline strength (0..1),
    // z = glow strength (0..1), w = vignette strength (0..1).
    params: vec4<f32>,
};
@group(2) @binding(2) var<uniform> crt: CrtU;

struct CrtOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Fullscreen triangle: 3 vertices covering clip space, UV in 0..1 (y-down to
// match the texture's top-left origin). Driven with draw(0..3), no vertex buffer.
@vertex fn vs_crt(@builtin(vertex_index) vid: u32) -> CrtOut {
    var o: CrtOut;
    // (-1,-1),(3,-1),(-1,3): the classic oversized triangle.
    let x = f32((vid << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vid & 2u) * 2.0 - 1.0;
    o.clip = vec4<f32>(x, y, 0.0, 1.0);
    // Map clip xy to 0..1 uv; flip y so uv.y=0 is the top of the image.
    o.uv = vec2<f32>(x * 0.5 + 0.5, 1.0 - (y * 0.5 + 0.5));
    return o;
}

// Apply a gentle barrel distortion to a centered (-1..1) coordinate. `amt` is
// the curvature strength; 0 returns the input unchanged.
fn crt_curve(p: vec2<f32>, amt: f32) -> vec2<f32> {
    // Push corners outward proportional to the squared distance along the other
    // axis — the canonical cheap CRT bulge.
    let off = p.yx * p.yx * amt;
    return p + p * off;
}

@fragment fn fs_crt(in: CrtOut) -> @location(0) vec4<f32> {
    let curvature = crt.params.x;
    let scan_amt = crt.params.y;
    let glow_amt = crt.params.z;
    let vig_amt = crt.params.w;

    // Barrel curvature: warp the sample coordinate around the screen center.
    var uv = in.uv;
    if (curvature > 0.0) {
        let centered = uv * 2.0 - 1.0;
        let warped = crt_curve(centered, curvature);
        uv = warped * 0.5 + 0.5;
        // Outside the curved screen is the bezel (black).
        if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
            return vec4<f32>(0.0, 0.0, 0.0, 1.0);
        }
    }

    let texel = 1.0 / u.screen.xy;
    var col = textureSample(crt_tex, crt_samp, uv).rgb;

    // Cheap glow: a few offset taps summed and weighted, added back as bloom so
    // bright glyphs bleed light into neighbouring pixels (the phosphor halo).
    if (glow_amt > 0.0) {
        var bloom = vec3<f32>(0.0);
        let r = texel * 1.5;
        bloom += textureSample(crt_tex, crt_samp, uv + vec2<f32>( r.x, 0.0)).rgb;
        bloom += textureSample(crt_tex, crt_samp, uv + vec2<f32>(-r.x, 0.0)).rgb;
        bloom += textureSample(crt_tex, crt_samp, uv + vec2<f32>(0.0,  r.y)).rgb;
        bloom += textureSample(crt_tex, crt_samp, uv + vec2<f32>(0.0, -r.y)).rgb;
        bloom *= 0.25;
        col += bloom * glow_amt;
    }

    // Scanlines: darken every other physical row in a smooth sine so the effect
    // reads as a real raster line rather than a hard 1px comb. Tied to the
    // physical pixel row (uv.y * height) so the line density is DPI-correct.
    if (scan_amt > 0.0) {
        let line = sin(uv.y * u.screen.y * 3.14159265);
        let scan = 1.0 - scan_amt * (0.5 - 0.5 * line);
        col *= scan;
        // Aperture-grille tint: a soft per-column RGB cycle so vertical triads
        // shimmer like a Trinitron, scaled down so it stays subtle.
        let triad = uv.x * u.screen.x;
        let grille = vec3<f32>(
            0.5 + 0.5 * cos(triad * 2.094395 + 0.0),
            0.5 + 0.5 * cos(triad * 2.094395 + 2.094395),
            0.5 + 0.5 * cos(triad * 2.094395 + 4.18879),
        );
        col *= mix(vec3<f32>(1.0), grille, scan_amt * 0.15);
    }

    // Vignette: gently darken toward the corners for the tube-glass falloff.
    if (vig_amt > 0.0) {
        let c = in.uv * 2.0 - 1.0;
        let v = 1.0 - vig_amt * dot(c, c) * 0.35;
        col *= clamp(v, 0.0, 1.0);
    }

    return vec4<f32>(col, 1.0);
}
