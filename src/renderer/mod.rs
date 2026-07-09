//! GPU-accelerated grid renderer: an instanced-quad pipeline pair with a glyph
//! atlas, driven by `crate::app`.
//!
//! Each frame draws two passes over a single static unit-quad vertex buffer:
//!   1. one solid-color background quad per visible cell, then
//!   2. one textured quad per glyph (sampled from a shared atlas texture).
//!
//! All coordinates are physical pixels; the vertex shaders project to NDC using
//! the surface size carried in the group(0) uniform.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::text::{CellMetrics, Text};

mod cell;
mod crt;
mod cursor_trail;
mod effect;
mod frame;
mod geometry;
mod image_draw;
mod init;
mod multipane;
mod opacity;
mod overlay;
mod pipeline;

pub use effect::WindowEffect;
pub use opacity::{opacity_to_slider, slider_to_opacity};

// Re-export geometry helpers so `use super::*` in sibling modules keeps working.
pub(crate) use geometry::{Packer, ScissorRect, clamp_scissor};
// Re-export font config struct for call sites outside the renderer module.
pub use init::RendererFontConfig;

#[cfg(test)]
mod tests;

/// XDG-cache directory for pipeline cache files.
fn pipeline_cache_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_else(|| std::ffi::OsString::from("/"));
            std::path::PathBuf::from(home).join(".cache")
        });
    base.join("glassy")
}

/// Save the pipeline cache bytes to disk atomically (write temp, rename over).
/// Failures are logged and silently swallowed; a missing cache is never fatal.
fn save_pipeline_cache(cache: &wgpu::PipelineCache, adapter_info: &wgpu::AdapterInfo) {
    let Some(key) = wgpu::util::pipeline_cache_key(adapter_info) else {
        return;
    };
    let data = match cache.get_data() {
        Some(d) if !d.is_empty() => d,
        _ => return,
    };
    let dir = pipeline_cache_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("glassy: pipeline cache dir create failed: {e}");
        return;
    }
    let path = dir.join(&key);
    let tmp = path.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, &data) {
        log::warn!("glassy: pipeline cache write failed: {e}");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        log::warn!("glassy: pipeline cache rename failed: {e}");
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    log::info!(
        "glassy: pipeline cache saved ({} B) → {:?}",
        data.len(),
        path
    );
}

/// Load raw pipeline cache bytes from disk (returns `None` on any error).
fn load_pipeline_cache_data(adapter_info: &wgpu::AdapterInfo) -> Option<Vec<u8>> {
    let key = wgpu::util::pipeline_cache_key(adapter_info)?;
    let path = pipeline_cache_dir().join(&key);
    std::fs::read(&path)
        .inspect(|d| {
            log::info!(
                "glassy: pipeline cache loaded ({} B) from {:?}",
                d.len(),
                path
            );
        })
        .ok()
}

/// Mask-atlas dimensions (square, single-channel R8). Holds the coverage masks
/// for ordinary text (ASCII, CJK, box-drawing, monochrome symbols) — the vast
/// majority of glyphs. R8 cuts this atlas's memory and per-glyph upload bandwidth
/// 4x versus the old RGBA8 atlas (1 MB instead of 4 MB) since a coverage mask
/// only needs one byte per pixel.
const ATLAS_SIZE: u32 = 1024;
/// Color-atlas dimensions (square, RGBA8). Color emoji are rasterized at a font
/// strike ≥ the cell height (≈64px on Retina) for crisp downscaling, so each one
/// is larger than a text glyph; 1024² (4 MB) holds ~450 emoji at 64px — comfortably
/// covers even emoji-dense sessions. On overflow, the overflowing glyph is
/// skipped for one frame and the clear + repack is deferred to the next frame
/// boundary (see `atlas_overflow_pending`) rather than happening mid-frame.
const COLOR_ATLAS_SIZE: u32 = 1024;
/// Image-atlas dimensions (square, RGBA8). Inline images (kitty graphics) are
/// packed here, kept separate from the glyph atlases so a large image can't evict
/// the font cache. On overflow the image cache is cleared and repacked.
/// 1024 (4 MB) is sufficient for typical inline images and cuts idle VRAM vs the
/// old 2048 (16 MB) — change to 2048 if you display many/large kitty images at once.
const IMAGE_ATLAS_SIZE: u32 = 1024;
/// 1px gap between packed glyphs to avoid bilinear bleed across neighbours.
const GLYPH_GAP: u32 = 1;
/// Initial instance-buffer capacity (in instances). An idle terminal at a
/// shell prompt uses ~80-150 instances; 512 covers that comfortably and
/// avoids the 4096×80-byte pre-alloc (328 KB) that was never used at idle.
/// The buffer grows dynamically on the first frame that needs more.
const INITIAL_INSTANCES: usize = 512;

/// Default window background opacity (the "glassy" namesake) when config/CLI do
/// not specify one: the alpha applied to the terminal's cell backgrounds and
/// clear color so the desktop shows through. 1.0 is fully opaque. Foreground
/// content (glyphs, box drawing, cursor, rules) stays fully opaque so text reads
/// crisply over the translucent backdrop. The effective value is configurable
/// and stored per-`Renderer` (`opacity`).
pub const DEFAULT_OPACITY: f32 = 0.92;

/// Default strength of the unfocused-pane dim overlay (`unfocused_dim`): the
/// alpha of a black quad composited over an unfocused split tile so the focused
/// pane reads as foreground. Chosen so the tile clearly recedes while its
/// content stays legible — 0.10 was too subtle to read as "dimmed".
pub const DEFAULT_PANE_DIM: f32 = 0.28;

/// Transient surface-acquisition outcomes for a frame.
///
/// wgpu 29 replaced the old `wgpu::SurfaceError` with the [`wgpu::CurrentSurfaceTexture`]
/// enum and no longer exposes a `SurfaceError` type. We surface the non-success
/// states through this small mirror so callers can decide whether to retry. The
/// `Lost`/`Outdated` cases are already self-healed (the surface is reconfigured)
/// before the error is returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceError {
    /// Acquisition timed out; skip this frame and try again later.
    Timeout,
    /// The window is occluded (minimized / fully covered); skip the frame.
    Occluded,
    /// Surface was lost or its configuration went stale. Already reconfigured here.
    Outdated,
    /// A validation error was raised during acquisition; attend to it and retry.
    Validation,
}

/// Result of [`Renderer::render`].
pub type RenderResult = std::result::Result<(), SurfaceError>;

/// group(0) uniform: surface size in physical px. `.zw` are unused padding so
/// the struct is a clean 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniform {
    screen: [f32; 4],
}

/// Per-cell background instance (slot 1 of the bg pipeline).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BgInstance {
    pos: [f32; 2],
    size: [f32; 2],
    color: [f32; 4],
}

/// Per-glyph foreground instance (slot 1 of the fg pipeline).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct FgInstance {
    pos: [f32; 2],
    size: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    color: [f32; 4],
    flags: u32,
    _pad: [u32; 3],
}

/// The instances belonging to a single grid row. Held persistently across frames
/// so a row whose terminal content did not change is reused verbatim instead of
/// being rebuilt and re-uploaded every frame.
#[derive(Default)]
pub(crate) struct RowInstances {
    bg: Vec<BgInstance>,
    fg: Vec<FgInstance>,
}

/// A glyph that has been packed into the atlas: its uv rect plus the placement
/// data needed to position the quad relative to the cell pen origin.
#[derive(Clone, Copy)]
pub(crate) struct AtlasGlyph {
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    px_w: f32,
    px_h: f32,
    left: i32,
    top: i32,
    is_color: bool,
}

/// The active underline style for a cell. Alacritty treats these as mutually
/// exclusive (the latest SGR wins), so we carry a single enum rather than a set
/// of booleans. `strikeout` is orthogonal and lives alongside in `Decorations`.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    Curl,
    Dotted,
    Dashed,
}

/// Text decorations (underline / strikethrough) requested for a cell. Straight
/// strokes are painted as solid rectangles via `push_solid`; the curly underline
/// is a dedicated foreground decoration instance with procedural coverage. The
/// `color` is the decoration color (SGR 58 underline color, or the cell fg when
/// no separate color is set) so e.g. a red LSP curl sits under default-fg text.
#[derive(Clone, Copy, Default)]
pub struct Decorations {
    pub underline: UnderlineStyle,
    pub strikeout: bool,
    /// SGR 53 overline: a stroke along the cell's TOP edge (mirrors the strikeout
    /// stroke but at the top). SGR 55 clears it. alacritty_terminal's `Flags` has
    /// no overline bit (and vte drops SGR 53/55), so glassy tracks overline in a
    /// side table (`OverlineMap`) and sets this flag in the render path.
    pub overline: bool,
    pub color: [f32; 4],
}

/// One cell in a ligature-shaped run, passed to [`Renderer::push_ligature_run`].
/// Carries the cell's position, foreground/background colors, and display flags,
/// but NOT the character — the run text is passed separately as `&str`.
#[derive(Clone, Copy)]
pub struct LigatureCell {
    /// Grid column of this cell.
    pub col: usize,
    /// Foreground color (straight RGBA).
    pub fg: [f32; 4],
    /// Background color (straight RGBA).
    pub bg: [f32; 4],
    /// True if this cell already occupies two columns (CJK wide / wide-emoji).
    pub wide: bool,
    /// Text decorations for this cell (underline, strikethrough, etc.).
    pub decorations: Decorations,
}

/// Cursor overlay shapes painted as solid rectangles by the renderer. The filled
/// block cursor is handled in the app by inverting the cell beneath it (so the
/// glyph stays legible), so it is intentionally absent here; this enum only covers
/// the shapes that draw on top of the cell as fg-colored bars or an outline.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CursorOverlay {
    /// Thin vertical bar at the cell's left edge.
    Beam,
    /// Short horizontal bar at the cell's bottom edge.
    Underline,
    /// Hollow outline box (four thin rails) around the cell. Used for the
    /// `HollowBlock` shape and for any shape while the window is unfocused.
    Hollow,
}

/// A rasterized glyph bitmap copied out of `Text` into owned storage, so the
/// `self.text` borrow ends before we touch the atlas/packer/queue.
pub(crate) struct Raster {
    width: u32,
    height: u32,
    left: i32,
    top: i32,
    is_color: bool,
    data: Vec<u8>,
}

/// Copy a slice of freshly-rasterized glyphs out of `Text` (dropping empties)
/// into owned `Raster`s, releasing the `Text` borrow for atlas packing.
fn collect_rasters(glyphs: &[crate::text::RasterizedGlyph]) -> Vec<Raster> {
    glyphs
        .iter()
        .filter(|r| r.width != 0 && r.height != 0)
        .map(|r| Raster {
            width: r.width,
            height: r.height,
            left: r.left,
            top: r.top,
            is_color: r.is_color,
            data: r.data.clone(),
        })
        .collect()
}

/// The window padding (grid inset) for a given cell height, in physical pixels.
/// A small symmetric margin (≈2-3 logical px) so glyphs don't kiss the window
/// edge — ghostty-style minimal, scaling gently with the cell. Applied on all sides.
fn pad_for(cell_height: f32) -> f32 {
    (cell_height * 0.15).round().max(4.0)
}

pub struct Renderer {
    // Keep the window alive for as long as the surface borrows it ('static surface).
    _window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    bg_pipeline: wgpu::RenderPipeline,
    fg_pipeline: wgpu::RenderPipeline,
    overlay_pipeline: wgpu::RenderPipeline,

    /// Pipeline cache handle. `Some` when the Vulkan backend supports
    /// `PIPELINE_CACHE`; `None` on other backends. Passed to all three
    /// `create_render_pipeline` calls. Saved to disk on exit via
    /// [`Renderer::save_pipeline_cache`].
    pipeline_cache: Option<wgpu::PipelineCache>,
    /// GPU adapter info, used to derive the cache file name on save.
    adapter_info: wgpu::AdapterInfo,

    unit_quad: wgpu::Buffer,

    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,

    // The R8 mask atlas (ordinary text) and the RGBA8 color atlas (emoji). The
    // bind group internally retains both texture views, the sampler, and the
    // layout it was built from, so we only keep the textures (for atlas writes)
    // and the bind group itself.
    mask_atlas_texture: wgpu::Texture,
    color_atlas_texture: wgpu::Texture,
    atlas_bind_group: wgpu::BindGroup,

    /// Dedicated RGBA8 atlas + bind group for inline images. The fg shader's
    /// color path (`flags == 1`) samples binding 2; this bind group puts the
    /// image atlas there so image quads reuse the fg pipeline unchanged.
    ///
    /// Both fields start as `None` and are allocated lazily on the first
    /// `draw_image` call. Sessions that never display an inline image never pay
    /// the 4 MB GPU texture allocation (IMAGE_ATLAS_SIZE² × 4 bytes).
    image_atlas_texture: Option<wgpu::Texture>,
    image_bind_group: Option<wgpu::BindGroup>,
    /// Bind group layout and sampler shared with the glyph atlas, kept alive so
    /// the image bind group can be created lazily without re-creating the layout.
    image_atlas_bind_group_layout: wgpu::BindGroupLayout,
    image_atlas_sampler: wgpu::Sampler,
    image_packer: Packer,
    /// Image id -> packed location in the image atlas (uploaded once per id).
    image_cache: HashMap<u32, AtlasGlyph>,
    /// This frame's image quads, rebuilt every frame from live placements and
    /// drawn as an overlay after the damage-tracked grid passes.
    image_overlay: Vec<FgInstance>,
    image_buffer: wgpu::Buffer,
    image_capacity: usize,
    image_count: u32,

    /// Translucent panel quads (modals / dropdown / context menu). Rebuilt every
    /// frame like the image overlay; drawn last with premultiplied blending so the
    /// terminal shows through. Empty (zero cost) whenever no panel is open.
    overlay_quads: Vec<BgInstance>,
    overlay_buffer: wgpu::Buffer,
    overlay_capacity: usize,
    overlay_count: u32,
    /// Text-on-glass: panel glyphs drawn AFTER the overlay quads (the fg grid pass
    /// runs before the overlay quads, so panel text pushed as normal cells would be
    /// occluded by the glass body). Mirrors the image overlay; uses the fg pipeline
    /// + the text atlas bind group. Rebuilt every frame, empty when no panel open.
    overlay_text: Vec<FgInstance>,
    overlay_text_buffer: wgpu::Buffer,
    overlay_text_capacity: usize,
    overlay_text_count: u32,
    /// Cached tab-bar overlay instances (quads + text), captured the last time the
    /// tab bar was rebuilt. On a frame where nothing tab-relevant changed the App
    /// replays this cache via [`Renderer::replay_tab_overlay`] instead of re-running
    /// the tab-bar painter (which shapes every tab title glyph). This is the second
    /// half of the split typing-lag fix: while typing, only the focused pane and its
    /// cells change, so the tab bar is identical frame-to-frame.
    tab_overlay_quads: Vec<BgInstance>,
    tab_overlay_text: Vec<FgInstance>,
    /// Marks the start offsets of the tab-bar region in the live overlay lists
    /// while it is being captured (between `begin_tab_overlay` and `commit`).
    tab_overlay_mark: Option<(usize, usize)>,

    /// Shelf packer for the R8 mask atlas.
    packer: Packer,
    /// Shelf packer for the RGBA8 color atlas.
    color_packer: Packer,
    /// Set when a glyph atlas overflowed and the deferred repack (see
    /// `atlas_overflow_pending`) will invalidate every cached glyph's UVs at
    /// the next frame boundary. The app must then force a full row rebuild so
    /// persisted rows don't keep stale UVs. Read via
    /// [`Renderer::pull_atlas_reset`].
    atlas_reset: bool,
    /// Set when a glyph atlas filled mid-frame. Unlike the old inline repack,
    /// the actual cache-clear + packer reset is DEFERRED to the next
    /// [`Renderer::begin_frame`] so no instance already emitted this frame keeps
    /// a UV pointing into an atlas region we are about to rewind (that was the
    /// scroll/emoji-density corruption). The overflowing glyph is skipped for one
    /// frame; `atlas_reset` (set alongside) forces the full rebuild that repacks it.
    atlas_overflow_pending: bool,
    glyph_cache: HashMap<(char, bool, bool), Vec<AtlasGlyph>>,
    /// Atlas entries for multi-codepoint grapheme clusters (combining/ZWJ).
    cluster_cache: HashMap<(String, bool, bool), Vec<AtlasGlyph>>,
    /// Atlas entries for ligature-shaped multi-cell runs, keyed by
    /// `(run_text, bold, italic)`. Each inner `Vec<Vec<AtlasGlyph>>` contains
    /// one entry per input character: non-empty for the first character of each
    /// shaped glyph, empty for ligature-continuation cells. Populated lazily by
    /// [`Renderer::ensure_run_glyphs`] and cleared on atlas reset.
    ligature_run_cache: HashMap<(String, bool, bool), Vec<Vec<AtlasGlyph>>>,
    /// Characters (single-cell path) whose shaped advance exceeds 1.1× `cell_w`.
    /// These glyphs are rendered in a 2-cell (WIDE) box even when alacritty did
    /// not flag them as wide — corrects Nerd-font icons that overflow their cell.
    wide_char_set: std::collections::HashSet<(char, bool, bool)>,
    /// Cache for [`Renderer::primary_font_covers`] — whether the PRIMARY font
    /// has its own glyph for `(char, bold, italic)`. Gates ligature-run
    /// eligibility (see `src/text/presentation.rs`); invalidated on font
    /// reload/resize alongside `glyph_cache` since coverage depends on which
    /// face is loaded, not on atlas state (so it is NOT cleared on atlas
    /// overflow repack).
    primary_coverage_cache: HashMap<(char, bool, bool), bool>,
    /// Whether the active font was detected to have an OpenType GSUB `liga`
    /// feature.  When false, ligature-run shaping is skipped regardless of the
    /// user config flag (no point paying for run-level shaping on a non-liga font).
    font_has_ligatures: bool,
    /// Whether the user config has ligature shaping enabled.
    ligatures_enabled: bool,

    text: Text,
    metrics: CellMetrics,
    pad: f32,
    /// Per-side padding overrides in physical px (from config). When set, these
    /// override the uniform `pad` for their respective sides. When `None`, the
    /// uniform `pad` is used.
    pad_top: Option<f32>,
    pad_bottom: Option<f32>,
    pad_left: Option<f32>,
    pad_right: Option<f32>,
    /// Extra vertical inset (physical px) reserved ABOVE the terminal grid for the
    /// real-GUI tab bar. The grid's first row starts at `pad + grid_origin_y`; the
    /// chrome paints into the band `[0, grid_origin_y)`. Zero when no GUI chrome is
    /// active (the default), so the legacy single-pane path is unchanged.
    grid_origin_y: f32,
    /// Transient horizontal shake offset (physical px) added to every grid cell's
    /// and the cursor's X origin. Non-zero only while a Power-Mode "screen rock" is
    /// in flight; zero at rest (so the legacy layout is byte-identical). Set via
    /// [`Renderer::set_grid_origin_x`]. The chrome/overlays are NOT shifted — only
    /// the terminal content — so the tab bar/status bar stay pinned during a shake.
    grid_origin_x: f32,
    /// Explicit padding override in physical px (from config). When `Some`, it is
    /// used verbatim instead of the cell-derived `pad_for`, and is preserved
    /// across runtime font resizes.
    pad_override: Option<f32>,
    /// The current font size in physical pixels (tracked so runtime resize can
    /// step it up/down and reload the font).
    font_px: f32,
    /// The resolved/requested font family name, kept so a runtime font resize can
    /// reload the same family at the new size. `None` uses the discovery default.
    font_family: Option<String>,
    /// OpenType font feature overrides from the config's `font_features` key,
    /// kept here so runtime font size changes re-apply the same features to the
    /// newly loaded face.
    font_features: Vec<String>,
    // --- FONTS stream: per-style overrides + symbol map + variation axes. ---
    /// Per-style family overrides (`font_bold` / `font_italic` / `font_bold_italic`).
    font_bold: Option<String>,
    font_italic: Option<String>,
    font_bold_italic: Option<String>,
    /// Codepoint routing map (`font_symbol_map`).
    font_symbol_map: Vec<crate::config::parse::SymbolMapEntry>,
    /// Variable-font axis settings (`font_variations`).
    font_variations: Vec<String>,

    /// Persistent per-row instance storage. Index `r` holds row `r`'s background
    /// and foreground instances; only the rows reported as changed are rewritten
    /// each frame (see [`Renderer::begin_row`]/[`Renderer::end_frame`]), the rest
    /// are reused verbatim. The vectors are sized to the grid height by
    /// [`Renderer::resize_grid`].
    rows: Vec<RowInstances>,
    /// The row currently being (re)built; pushes from `push_cell` and friends land
    /// here. Set by [`Renderer::begin_row`].
    cur_row: usize,
    /// Per-row instance offsets (in instances, not bytes) describing the previous
    /// upload's layout, so an unchanged-layout frame can `write_buffer` just the
    /// dirty rows' sub-ranges. `len() == rows.len() + 1` (the last entry is the
    /// total). Empty means "layout unknown — do a full reflatten + upload".
    bg_row_offsets: Vec<u32>,
    fg_row_offsets: Vec<u32>,
    /// Persistent scratch for building each frame's prefix-sum offsets, swapped
    /// into `*_row_offsets` on a layout change — avoids a per-frame Vec alloc.
    bg_scratch_offsets: Vec<u32>,
    fg_scratch_offsets: Vec<u32>,
    /// Rows rebuilt via [`Renderer::begin_row`] this frame, in call order. Used to
    /// upload just those rows when the buffer layout is otherwise unchanged.
    dirty_rows: Vec<usize>,
    /// Flattened upload scratch: the concatenation of every row's instances in row
    /// order, rebuilt from `rows` when offsets shift. Kept to avoid reallocating.
    bg_flat: Vec<BgInstance>,
    fg_flat: Vec<FgInstance>,
    /// Total instance counts for the current frame's draw calls.
    bg_count: u32,
    fg_count: u32,
    bg_buffer: wgpu::Buffer,
    fg_buffer: wgpu::Buffer,
    bg_capacity: usize,
    fg_capacity: usize,

    clear_color: [f32; 4],

    /// Visual-bell flash overlay: a non-premultiplied straight RGBA color blended
    /// over the clear color and every cell background while a bell flash is
    /// active, or `None` when not flashing. The alpha is the blend strength.
    flash: Option<[f32; 4]>,

    /// Window background opacity in [0, 1]; applied to cell backgrounds and the
    /// clear color (premultiplied) when the surface is transparent.
    opacity: f32,

    /// Alpha of the dark overlay laid over unfocused panes in a split, in
    /// [0, 0.9]. 0 disables the overlay entirely. Configurable via
    /// `unfocused_dim`; see [`Renderer::set_pane_dim`].
    pane_dim: f32,

    /// Whether window opacity also applies to terminal text
    /// (`opacity_scope = text`). Default false: glyphs composite opaque so text
    /// stays crisp over the translucent backdrop. See [`Renderer::set_text_opacity`].
    text_opacity: bool,

    /// Whether the surface alpha mode actually composites alpha (a transparent
    /// window). When false we keep backgrounds fully opaque so a compositor that
    /// can't do translucency doesn't darken the window via premultiplied RGB.
    transparent: bool,

    /// Whether the surface uses premultiplied alpha (`PreMultiplied` mode, used
    /// on Vulkan/Linux). When false the surface uses straight alpha (`PostMultiplied`
    /// mode, used on Metal/macOS): `glass_bg` must NOT premultiply the RGB
    /// channels in that case or the compositor will double-multiply them.
    premultiplied_surface: bool,

    /// Multi-pane (split) render path. Empty/idle on the single-grid fast path;
    /// populated only between [`Renderer::begin_multi_frame`] and
    /// [`Renderer::render_multi`]. Splitting is rare, so this path fully rebuilds
    /// each frame (no per-row damage tracking) for simplicity.
    mp: MultiPane,

    // --- gpu-fx stream additions (cursor trail + CRT post-process). ---
    /// The shader module compiled from `shader.wgsl`, retained so the CRT post
    /// pipeline can be built lazily (only when the effect is enabled) from the
    /// same module rather than recompiling. The grid pipelines already captured
    /// their entry points at startup.
    crt_shader: wgpu::ShaderModule,
    /// The group(0) screen-size uniform bind group layout, retained so the CRT
    /// post pipeline (built lazily) can reuse it for the resolution uniform.
    uniform_bind_group_layout: wgpu::BindGroupLayout,
    /// CRT / glow / scanline post-process state (config `crt_effect`, default
    /// off). Entirely dormant — no GPU allocation, zero per-frame cost — until
    /// enabled. See [`crt::CrtPass`].
    crt: crt::CrtPass,
    /// Cursor trail / smear animation state (config `cursor_trail`, default off).
    /// Idle-safe: only animates while the cursor is mid-glide. See
    /// [`cursor_trail::CursorTrail`].
    cursor_trail: cursor_trail::CursorTrail,
    /// The currently-selected window post-process effect (config `window_effect`,
    /// default `None`). `None` keeps the zero-cost direct path; every other mode
    /// routes the grid through the shared CRT post pass with mode-specific params.
    /// See [`effect::WindowEffect`].
    window_effect: WindowEffect,

    /// Set by the `wgpu::Device::set_device_lost_callback` registered in
    /// [`Renderer::new_with_fonts`] (see `renderer/init.rs`) when the GPU
    /// device is lost after startup (driver crash/reset, GPU hot-unplug) — an
    /// event wgpu otherwise has no documented default behavior for on the next
    /// `submit()`/`present()`. The render loop checks [`Renderer::device_lost`]
    /// each frame and degrades gracefully (same clean-exit path as an init
    /// failure) instead of relying on that undocumented default. `Arc` so the
    /// callback closure (which may run on an arbitrary wgpu-internal thread)
    /// can set it without borrowing the `Renderer`.
    ///
    /// `#[allow(dead_code)]`: wiring the render loop's per-frame check is a
    /// separate app-level change outside this stream's scope (see
    /// `Renderer::device_lost`'s doc comment) — this field and its accessor
    /// are the additive, self-contained half.
    #[allow(dead_code)]
    device_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// State for the multi-pane (split) render path. A flat instance list whose
/// every quad already carries its ABSOLUTE surface-pixel position (pane pixel
/// origin + cell offset), plus one scissored draw per pane so a pane never
/// paints outside its region. Kept entirely separate from the single-grid
/// `rows`/`bg_buffer`/`fg_buffer` machinery so the fast path is untouched.
#[derive(Default)]
struct MultiPane {
    /// Per-pane draw records (scissor rect + the instance sub-ranges to draw).
    panes: Vec<PaneDraw>,
    /// Flattened background instances for all panes, in pane order.
    bg: Vec<BgInstance>,
    /// Flattened foreground instances for all panes, in pane order.
    fg: Vec<FgInstance>,
    /// The pane currently being built (its origin + scissor), if any.
    cur: Option<PaneBuild>,
    /// Instance buffers + capacities for this path (grown on demand).
    bg_buffer: Option<wgpu::Buffer>,
    fg_buffer: Option<wgpu::Buffer>,
    bg_capacity: usize,
    fg_capacity: usize,
    /// Per-pane instance cache, keyed by pane id. A pane whose grid did not change
    /// this frame is reused verbatim from here (its already-origin-translated bg/fg
    /// instances) instead of re-locking the term, re-iterating cells and re-shaping
    /// glyphs — the dominant cost that made typing in a split lag. Only the pane
    /// whose content changed is rebuilt via `begin_pane`/`push`/`end_pane`.
    cache: HashMap<usize, CachedPane>,
    /// True when at least one pane was reused from cache this frame (so the flat
    /// upload is still required but the expensive CPU rebuild was skipped).
    reused_any: bool,
}

/// A finished pane's instances, cached across frames so an unchanged pane is
/// re-emitted without re-running the (expensive) `push_pane` CPU path.
#[derive(Default, Clone)]
struct CachedPane {
    /// Origin-translated background instances (absolute surface pixels).
    bg: Vec<BgInstance>,
    /// Origin-translated foreground instances (absolute surface pixels).
    fg: Vec<FgInstance>,
    /// The pane's clamped scissor.
    scissor: ScissorRect,
    /// Whether this pane is dimmed (an unfocused tile while `dim_unfocused` is on);
    /// when true a subtle dark overlay quad is emitted over the pane after its
    /// glyphs. Cached so a reused pane keeps its dim without a rebuild.
    dimmed: bool,
}

/// A finished pane's draw record: the GPU scissor rectangle and the half-open
/// instance ranges (into `MultiPane::bg`/`fg`) that belong to this pane.
#[derive(Clone, Copy)]
struct PaneDraw {
    scissor: ScissorRect,
    bg_start: u32,
    bg_end: u32,
    fg_start: u32,
    fg_end: u32,
}

/// The pane being assembled between `begin_pane` and `end_pane`.
#[derive(Clone, Copy)]
struct PaneBuild {
    /// Pane id (cache key).
    id: usize,
    /// Absolute pixel origin (top-left) added to every pushed quad.
    origin: [f32; 2],
    /// Clamped scissor for this pane.
    scissor: ScissorRect,
    /// Whether to draw the accent focus border for this pane.
    focused: bool,
    /// Whether to dim this pane's content with a subtle dark overlay quad (drawn
    /// after the cells). Set for unfocused panes when `dim_unfocused` is on; the
    /// dim is part of the cached pane so reused frames stay dimmed.
    dimmed: bool,
}
