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
#[allow(dead_code)] // wired in by app.rs in a subsequent wave (call on exit)
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
    log::info!("glassy: pipeline cache saved ({} B) → {:?}", data.len(), path);
}

/// Load raw pipeline cache bytes from disk (returns `None` on any error).
fn load_pipeline_cache_data(adapter_info: &wgpu::AdapterInfo) -> Option<Vec<u8>> {
    let key = wgpu::util::pipeline_cache_key(adapter_info)?;
    let path = pipeline_cache_dir().join(&key);
    std::fs::read(&path)
        .inspect(|d| {
            log::info!("glassy: pipeline cache loaded ({} B) from {:?}", d.len(), path);
        })
        .ok()
}

/// Mask-atlas dimensions (square, single-channel R8). Holds the coverage masks
/// for ordinary text (ASCII, CJK, box-drawing, monochrome symbols) — the vast
/// majority of glyphs. R8 cuts this atlas's memory and per-glyph upload bandwidth
/// 4x versus the old RGBA8 atlas (1 MB instead of 4 MB) since a coverage mask
/// only needs one byte per pixel.
const ATLAS_SIZE: u32 = 1024;
/// Color-atlas dimensions (square, RGBA8). Only color glyphs (emoji) live here,
/// so it can be much smaller than the mask atlas; on overflow the shared
/// full-atlas path clears both caches and repacks.
const COLOR_ATLAS_SIZE: u32 = 256;
/// Image-atlas dimensions (square, RGBA8). Inline images (kitty graphics) are
/// packed here, kept separate from the glyph atlases so a large image can't evict
/// the font cache. On overflow the image cache is cleared and repacked.
/// 1024 (4 MB) is sufficient for typical inline images and cuts idle VRAM vs the
/// old 2048 (16 MB) — change to 2048 if you display many/large kitty images at once.
const IMAGE_ATLAS_SIZE: u32 = 1024;
/// 1px gap between packed glyphs to avoid bilinear bleed across neighbours.
const GLYPH_GAP: u32 = 1;
/// Initial instance-buffer capacity (in instances) so the first `cast_slice`
/// is never empty and we rarely reallocate.
const INITIAL_INSTANCES: usize = 4096;

/// Default window background opacity (the "glassy" namesake) when config/CLI do
/// not specify one: the alpha applied to the terminal's cell backgrounds and
/// clear color so the desktop shows through. 1.0 is fully opaque. Foreground
/// content (glyphs, box drawing, cursor, rules) stays fully opaque so text reads
/// crisply over the translucent backdrop. The effective value is configurable
/// and stored per-`Renderer` (`opacity`).
pub const DEFAULT_OPACITY: f32 = 0.92;

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
struct FgInstance {
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
struct RowInstances {
    bg: Vec<BgInstance>,
    fg: Vec<FgInstance>,
}

/// A glyph that has been packed into the atlas: its uv rect plus the placement
/// data needed to position the quad relative to the cell pen origin.
#[derive(Clone, Copy)]
struct AtlasGlyph {
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
    pub color: [f32; 4],
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
struct Raster {
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
/// Scales with the cell so a larger font keeps proportional breathing room.
fn pad_for(cell_height: f32) -> f32 {
    (cell_height * 0.35).round().max(4.0)
}

/// Simple shelf packer state for an atlas texture of side `size`.
struct Packer {
    size: u32,
    cursor_x: u32,
    cursor_y: u32,
    shelf_height: u32,
}

impl Packer {
    fn new(size: u32) -> Self {
        Self {
            size,
            cursor_x: 0,
            cursor_y: 0,
            shelf_height: 0,
        }
    }

    fn reset(&mut self) {
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.shelf_height = 0;
    }

    /// Reserve a `w`x`h` region. Returns its top-left origin, or `None` if the
    /// atlas is full (caller should clear the cache and retry).
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > self.size || h > self.size {
            return None;
        }
        // Wrap to a new shelf if the glyph doesn't fit the current row.
        if self.cursor_x + w > self.size {
            self.cursor_y += self.shelf_height + GLYPH_GAP;
            self.cursor_x = 0;
            self.shelf_height = 0;
        }
        if self.cursor_y + h > self.size {
            return None;
        }
        let origin = (self.cursor_x, self.cursor_y);
        self.cursor_x += w + GLYPH_GAP;
        self.shelf_height = self.shelf_height.max(h);
        Some(origin)
    }
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
    #[allow(dead_code)] // read by save_pipeline_cache, wired in by app.rs later
    pipeline_cache: Option<wgpu::PipelineCache>,
    /// GPU adapter info, used to derive the cache file name on save.
    #[allow(dead_code)] // read by save_pipeline_cache, wired in by app.rs later
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
    image_atlas_texture: wgpu::Texture,
    image_bind_group: wgpu::BindGroup,
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
    /// Set when a glyph atlas overflowed and was repacked mid-frame (which
    /// invalidates every cached glyph's UVs). The app must then force a full
    /// row rebuild so persisted rows don't keep stale UVs. Read via
    /// [`Renderer::pull_atlas_reset`].
    atlas_reset: bool,
    glyph_cache: HashMap<(char, bool, bool), Vec<AtlasGlyph>>,
    /// Atlas entries for multi-codepoint grapheme clusters (combining/ZWJ).
    cluster_cache: HashMap<(String, bool, bool), Vec<AtlasGlyph>>,

    text: Text,
    metrics: CellMetrics,
    pad: f32,
    /// Extra vertical inset (physical px) reserved ABOVE the terminal grid for the
    /// real-GUI tab bar. The grid's first row starts at `pad + grid_origin_y`; the
    /// chrome paints into the band `[0, grid_origin_y)`. Zero when no GUI chrome is
    /// active (the default), so the legacy single-pane path is unchanged.
    grid_origin_y: f32,
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

    /// Whether the surface alpha mode actually composites alpha (a transparent
    /// window). When false we keep backgrounds fully opaque so a compositor that
    /// can't do translucency doesn't darken the window via premultiplied RGB.
    transparent: bool,

    /// Multi-pane (split) render path. Empty/idle on the single-grid fast path;
    /// populated only between [`Renderer::begin_multi_frame`] and
    /// [`Renderer::render_multi`]. Splitting is rare, so this path fully rebuilds
    /// each frame (no per-row damage tracking) for simplicity.
    mp: MultiPane,
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
}

/// A GPU scissor rectangle in surface pixels: an unsigned origin + extent,
/// already clamped to the surface so `set_scissor_rect` never rejects it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct ScissorRect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// Clamp an integer pixel rect (which may be partly off-surface or have a
/// negative origin) to the `surface_w` x `surface_h` bounds, yielding a scissor
/// the GPU will accept. A rect fully outside the surface clamps to zero extent.
/// Pure geometry (no GPU state) so it is unit-tested directly.
fn clamp_scissor(x: i32, y: i32, w: i32, h: i32, surface_w: u32, surface_h: u32) -> ScissorRect {
    // Left/top edges clamped to [0, surface]; right/bottom edges likewise, then
    // the extent is the (non-negative) difference.
    let x0 = x.max(0).min(surface_w as i32);
    let y0 = y.max(0).min(surface_h as i32);
    let x1 = (x + w.max(0)).max(0).min(surface_w as i32);
    let y1 = (y + h.max(0)).max(0).min(surface_h as i32);
    ScissorRect {
        x: x0 as u32,
        y: y0 as u32,
        w: (x1 - x0).max(0) as u32,
        h: (y1 - y0).max(0) as u32,
    }
}

impl Renderer {
    pub fn new(
        window: Arc<Window>,
        font_family: Option<String>,
        font_px: f32,
        opacity: f32,
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
        let gpu_thread = std::thread::spawn(move || -> anyhow::Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference,
                force_fallback_adapter: false,
                compatible_surface: Some(surface_arc_thread.as_ref()),
            }))
            .context("requesting GPU adapter")?;
            // Request PIPELINE_CACHE if the adapter supports it (Vulkan only today).
            // We leave all other features/limits at defaults so the device request never
            // fails on a feature-limited adapter.
            let supports_pipeline_cache = adapter
                .features()
                .contains(wgpu::Features::PIPELINE_CACHE);
            let required_features = if supports_pipeline_cache {
                wgpu::Features::PIPELINE_CACHE
            } else {
                wgpu::Features::empty()
            };
            let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
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
        });

        // Font load runs concurrently with the GPU thread above.
        let (text, metrics) =
            Text::load(font_family.as_deref(), font_px).context("loading font and cell metrics")?;
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
        let pipeline_cache: Option<wgpu::PipelineCache> = if device
            .features()
            .contains(wgpu::Features::PIPELINE_CACHE)
        {
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

        // Window translucency: prefer PreMultiplied so the surface's alpha is
        // composited against the desktop. We emit premultiplied colors (RGB
        // already scaled by alpha), which is exactly what PreMultiplied expects
        // and also matches the foreground pass's premultiplied blending. If the
        // compositor doesn't offer it (can't do translucency), fall back to its
        // first mode and stay fully opaque.
        let transparent = caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied);
        let alpha_mode = if transparent {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else {
            caps.alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto)
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

        // Dedicated image atlas + bind group, sharing the atlas layout/sampler.
        // The image atlas view occupies both the mask (0) and color (2) slots;
        // image quads use `flags == 1`, so only slot 2 is actually sampled.
        let image_atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
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
        let image_atlas_view =
            image_atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let image_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image-bg"),
            layout: &atlas_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&image_atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&atlas_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&image_atlas_view),
                },
            ],
        });

        // --- Shader + pipelines. ---
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glassy-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
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
                    // (1 - src.a). With glyph alpha 1.0 the text stays fully opaque
                    // and crisp regardless of the window opacity.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
            image_atlas_texture,
            image_bind_group,
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
            text,
            metrics,
            pad: pad_for(metrics.height),
            grid_origin_y: 0.0,
            pad_override: None,
            font_px,
            font_family,
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
            mp: MultiPane::default(),
        };

        // Pre-warm the atlas with printable ASCII (regular + bold) so the first
        // frame is rasterize-free. Bold is prewarm'd because most shell prompts and
        // editors immediately render bold text; without it there is a visible
        // first-frame stutter when the first bold glyph triggers an atlas upload.
        for byte in 0x20u8..=0x7E {
            renderer.ensure_glyphs(byte as char, false, false);
            renderer.ensure_glyphs(byte as char, true, false);
        }
        log::info!("  renderer: ascii prewarm done {:.1} ms (total Renderer::new)", ms(t));

        Ok(renderer)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        // Skip the (expensive) surface.configure() call when the size is unchanged.
        // This is common at startup when the app calls resize() twice: once with the
        // initial 1×1 placeholder and once with the real window size, plus any
        // redundant resize events the compositor fires. configure() can stall the
        // driver for a swapchain recreation even when the size hasn't changed.
        if width == self.config.width && height == self.config.height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&Uniform {
                screen: [width as f32, height as f32, 0.0, 0.0],
            }),
        );
    }

    pub fn cell_metrics(&self) -> CellMetrics {
        self.metrics
    }

    /// Physical-pixel inset applied to the grid on all sides. The app must
    /// account for this when computing how many cells fit in the surface.
    pub fn pad(&self) -> f32 {
        self.pad
    }

    /// The current font size in physical pixels.
    pub fn font_px(&self) -> f32 {
        self.font_px
    }

    /// The current surface size in physical pixels `(width, height)`. Used by the
    /// split-pane layout to tile the full surface below the tab strip.
    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Override the grid padding (inset) with an explicit physical-pixel value,
    /// preserved across runtime font resizes. The caller must recompute the grid.
    pub fn set_pad(&mut self, pad: f32) {
        let pad = pad.max(0.0);
        self.pad_override = Some(pad);
        self.pad = pad;
    }

    /// Set the window background opacity at runtime. Only has a visible effect on
    /// a transparent surface (the compositor must composite alpha); the caller
    /// should trigger a full redraw afterward so every cell background repaints.
    pub fn set_opacity(&mut self, opacity: f32) {
        self.opacity = opacity.clamp(0.0, 1.0);
    }

    /// Reload the font at a new physical pixel size, recomputing the cell metrics
    /// and padding and rebuilding the glyph atlas. On failure the previous font
    /// is retained (the error is logged) so a bad size never breaks rendering.
    ///
    /// The caller is responsible for re-querying `cell_metrics()`/`pad()` and
    /// resizing the PTY grid afterward (the renderer does not know the grid).
    pub fn set_font_size(&mut self, font_px: f32) {
        let font_px = font_px.clamp(4.0, 300.0);
        if (font_px - self.font_px).abs() < 0.01 {
            return;
        }
        let (text, metrics) = match Text::load(self.font_family.as_deref(), font_px) {
            Ok(loaded) => loaded,
            Err(e) => {
                log::warn!("glassy: font resize to {font_px:.1}px failed: {e:#}");
                return;
            }
        };
        self.text = text;
        self.metrics = metrics;
        self.pad = self.pad_override.unwrap_or_else(|| pad_for(metrics.height));
        self.font_px = font_px;

        // The atlases hold glyphs rasterized at the old size; reset both packers
        // and caches so glyphs are re-rasterized at the new size on demand.
        self.packer.reset();
        self.color_packer.reset();
        self.glyph_cache.clear();
        self.cluster_cache.clear();

        // Pre-warm printable ASCII (regular + bold) at the new size for a
        // rasterize-free first frame after a font-size change.
        for byte in 0x20u8..=0x7E {
            self.ensure_glyphs(byte as char, false, false);
            self.ensure_glyphs(byte as char, true, false);
        }
    }

    /// Take the "atlas was repacked" flag (clearing it). When true, the caller
    /// should force a full row rebuild + repaint so no row keeps stale glyph UVs.
    pub fn pull_atlas_reset(&mut self) -> bool {
        std::mem::take(&mut self.atlas_reset)
    }

    /// Size the persistent per-row instance storage to `rows` grid rows, clearing
    /// every row. The app calls this whenever the grid height changes (resize /
    /// font resize) so subsequent per-row rebuilds index a correctly-sized table.
    /// Forces a full re-upload on the next `end_frame` (offsets are invalidated).
    pub fn resize_grid(&mut self, rows: usize) {
        // Drop or grow to exactly `rows` entries, clearing all so a fresh full
        // rebuild populates them. `RowInstances::default` is empty.
        self.rows.clear();
        self.rows.resize_with(rows, RowInstances::default);
        // Offsets no longer describe the buffer; clearing them forces end_frame to
        // reflatten and reupload everything.
        self.bg_row_offsets.clear();
        self.fg_row_offsets.clear();
        self.dirty_rows.clear();
    }

    /// Begin a frame: set the clear color. Unlike a full rebuild this does NOT
    /// clear the per-row instance storage — only the rows the app re-pushes via
    /// [`Renderer::begin_row`] are rewritten; the rest are reused from last frame.
    pub fn begin_frame(&mut self, default_bg: [f32; 4]) {
        // The clear color paints the (translucent) window backdrop, so it takes
        // the window opacity (and the visual-bell flash) just like the per-cell
        // default-background quads.
        self.clear_color = self.glass_bg(default_bg);
        // Image quads are not damage-tracked; they are rebuilt from live
        // placements every frame, so clear last frame's overlay here.
        self.image_overlay.clear();
        // Panel overlay (modals / menus) is likewise rebuilt every frame.
        self.overlay_quads.clear();
        self.overlay_text.clear();
    }

    /// Queue an inline image to be drawn this frame at pixel rect
    /// `(x, y, dst_w, dst_h)`. `rgba` is straight-alpha RGBA8, `img_w`x`img_h`.
    /// The pixels are uploaded into the image atlas once per `id` and cached;
    /// subsequent frames only push a quad. Oversized images (larger than the
    /// atlas) are skipped.
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
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.image_atlas_texture,
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

    /// Begin (re)building grid row `row`: subsequent `push_cell`/`push_cursor`
    /// calls land in this row's instance storage, replacing its previous contents.
    /// Out-of-range rows are ignored (clamped to a scratch slot) so a stale cursor
    /// row past a shrink never panics.
    pub fn begin_row(&mut self, row: usize) {
        if row >= self.rows.len() {
            // Should not happen if the app keeps the grid in sync, but stay safe:
            // grow so the index is valid rather than panicking mid-frame.
            self.rows.resize_with(row + 1, RowInstances::default);
            self.bg_row_offsets.clear();
            self.fg_row_offsets.clear();
        }
        self.cur_row = row;
        self.rows[row].bg.clear();
        self.rows[row].fg.clear();
        self.dirty_rows.push(row);
    }

    /// Re-target pushes at an already-built row WITHOUT clearing it, so callers can
    /// append to a row after its cells were laid down (the cursor overlay is pushed
    /// this way so it lands on top of the cursor row's cell backgrounds). The row
    /// must already have been built via [`Renderer::begin_row`] this frame.
    pub fn set_cur_row(&mut self, row: usize) {
        if row < self.rows.len() {
            self.cur_row = row;
        }
    }

    /// Apply the window opacity to a cell-background color and premultiply it.
    /// Backgrounds become translucent (alpha = `self.opacity`) so the desktop shows
    /// through; the RGB is premultiplied to match the PreMultiplied surface and
    /// the foreground pass's premultiplied blending. A no-op (and fully opaque)
    /// when the compositor can't do translucency.
    fn glass_bg(&self, color: [f32; 4]) -> [f32; 4] {
        let color = self.apply_flash(color);
        if !self.transparent {
            return color;
        }
        let a = color[3] * self.opacity;
        [color[0] * a, color[1] * a, color[2] * a, a]
    }

    /// Blend the active visual-bell flash (straight RGBA over) onto a straight
    /// (non-premultiplied) background color, preserving its alpha. A no-op when no
    /// flash is active. Applied to cell backgrounds and the clear color so the
    /// whole window tints uniformly toward the flash color for the flash window.
    fn apply_flash(&self, color: [f32; 4]) -> [f32; 4] {
        match self.flash {
            None => color,
            Some([fr, fg, fb, fa]) => [
                color[0] + (fr - color[0]) * fa,
                color[1] + (fg - color[1]) * fa,
                color[2] + (fb - color[2]) * fa,
                color[3],
            ],
        }
    }

    /// Set (or clear) the visual-bell flash overlay color. `Some([r, g, b, a])`
    /// blends that straight RGBA over the background for the next frames; `None`
    /// restores the normal appearance. The caller drives the flash duration.
    pub fn set_flash(&mut self, flash: Option<[f32; 4]>) {
        self.flash = flash;
    }

    /// Save the wgpu pipeline cache to disk. Call once before the application
    /// exits so the next launch can skip shader compilation. Failures are logged
    /// but never fatal. On backends that don't support `PIPELINE_CACHE` this is
    /// a no-op.
    #[allow(dead_code)] // public API; wired in by app.rs in a subsequent wave
    pub fn save_pipeline_cache(&self) {
        if let Some(cache) = &self.pipeline_cache {
            save_pipeline_cache(cache, &self.adapter_info);
        }
    }

    /// Draw a thin scrollbar thumb in the right gutter of the terminal grid.
    ///
    /// `scroll_off` is the current scrollback offset (0 = at bottom, positive =
    /// scrolled up by that many rows).  `scroll_hist` is the total history rows
    /// available. `visible_rows` is the number of rows currently on screen.
    /// `surface_h` is the surface height in physical pixels.
    ///
    /// The thumb is painted via `push_overlay_px` (premultiplied-blend overlay
    /// pipeline) so it composites over the terminal content without touching the
    /// cell data.  It is 2 px wide in the right-most gutter, invisible (no-op)
    /// when there is nothing to scroll.
    #[allow(dead_code)] // public API; wired in by app.rs in a subsequent wave
    pub fn push_scrollbar_thumb(
        &mut self,
        scroll_off: usize,
        scroll_hist: usize,
        visible_rows: usize,
        surface_h: f32,
        color: [f32; 4],
    ) {
        let total = visible_rows + scroll_hist;
        if total <= visible_rows || surface_h <= 0.0 {
            return;
        }
        let track_h = surface_h;
        let thumb_h = (track_h * visible_rows as f32 / total as f32)
            .round()
            .max(4.0)
            .min(track_h);
        // scroll_off=0 → thumb at bottom; scroll_off=scroll_hist → thumb at top.
        let max_off = scroll_hist as f32;
        let thumb_y = if scroll_hist == 0 {
            track_h - thumb_h
        } else {
            let frac = scroll_off as f32 / max_off;
            (track_h - thumb_h) * (1.0 - frac)
        };
        let sw = self.config.width as f32;
        let thumb_w = 2.0_f32.max(1.0);
        let x = sw - thumb_w - 1.0; // 1 px margin from window edge
        let y = thumb_y.max(0.0).min(track_h - thumb_h);
        self.push_overlay_px(x, y, thumb_w, thumb_h, color);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push_cell(
        &mut self,
        col: usize,
        row: usize,
        ch: char,
        combiners: &[char],
        fg: [f32; 4],
        bg: [f32; 4],
        bold: bool,
        italic: bool,
        wide: bool,
        decorations: Decorations,
    ) {
        let cell_w = self.metrics.width;
        let cell_h = self.metrics.height;
        let pad = self.pad;
        // Grid origin of this cell, offset by the window padding (inset) and the
        // GUI tab-bar inset (`grid_origin_y`, zero in the multi-pane path where
        // each pane carries its own pixel origin).
        let origin_x = col as f32 * cell_w + pad;
        let origin_y = row as f32 * cell_h + pad + self.grid_origin_y;

        // A double-width (CJK / wide-emoji) cell occupies two columns: its advance
        // box spans `2 * cell_w`. The grid skips the trailing spacer cell, so we
        // lay the glyph out across the full two-cell box here. Single-width cells
        // keep the ordinary one-cell box.
        let box_w = if wide { cell_w * 2.0 } else { cell_w };

        // Push the cell background, but skip cells whose background equals the
        // frame's clear color — the clear already paints those, so emitting a quad
        // for every default cell is pure overdraw (the common case is most of the
        // grid). Decorations and procedural box/block segments are separate
        // instances pushed afterward, so they are unaffected by the skip. A wide
        // cell's background spans both columns. Backgrounds take the window opacity
        // (premultiplied) so the desktop shows through uniformly.
        let glass = self.glass_bg(bg);
        if glass != self.clear_color {
            self.rows[self.cur_row].bg.push(BgInstance {
                pos: [origin_x, origin_y],
                size: [box_w, cell_h],
                color: glass,
            });
        }

        // Underline / strikethrough strokes span the full cell width so they join
        // seamlessly across adjacent decorated cells. Pushed here (after the cell
        // background, in the bg pass) so they paint over the background in the
        // decoration color; glyphs draw on top in the later fg pass. The curly
        // underline is emitted as a foreground decoration instance instead.
        self.draw_decorations(origin_x, origin_y, decorations);

        let baseline = origin_y + self.metrics.ascent;

        // Grapheme cluster path: a base char with combining marks / ZWJ-joined
        // codepoints (e.g. compound emoji like the trans flag). Shape the whole
        // cluster as one unit so it resolves to its single combined glyph.
        if !combiners.is_empty() {
            let mut cluster = String::with_capacity(ch.len_utf8() + combiners.len() * 4);
            cluster.push(ch);
            cluster.extend(combiners.iter());
            self.ensure_cluster_glyphs(&cluster, bold, italic);
            if let Some(glyphs) = self.cluster_cache.get(&(cluster, bold, italic)) {
                let cur = self.cur_row;
                Self::place_glyphs(
                    &mut self.rows[cur].fg,
                    glyphs,
                    fg,
                    origin_x,
                    baseline,
                    cell_w,
                    box_w,
                    cell_h,
                );
            }
            return;
        }

        if ch == ' ' || ch == '\0' {
            return;
        }

        // PROCEDURAL box-drawing / block elements. Drawing these as font glyphs
        // leaves hairline gaps between adjacent cells (lines fail to connect), so
        // we paint them as solid foreground rectangles that span the full cell.
        // These quads are pushed AFTER this cell's background quad, so in the
        // painter-order bg pass they draw on top of it in the foreground color.
        let cp = ch as u32;
        let is_box = (0x2500..=0x257F).contains(&cp);
        let is_block = (0x2580..=0x259F).contains(&cp);
        if is_block {
            self.draw_block(ch, origin_x, origin_y, fg, bg);
            return;
        }
        if is_box {
            // `draw_box` returns false for code points it does not handle, in
            // which case we fall through to the normal glyph path so nothing
            // renders blank.
            if self.draw_box(ch, origin_x, origin_y, fg) {
                return;
            }
        }

        self.ensure_glyphs(ch, bold, italic);
        if let Some(glyphs) = self.glyph_cache.get(&(ch, bold, italic)) {
            let cur = self.cur_row;
            Self::place_glyphs(
                &mut self.rows[cur].fg,
                glyphs,
                fg,
                origin_x,
                baseline,
                cell_w,
                box_w,
                cell_h,
            );
        }
    }

    /// Compute the on-screen quad placement (`pos`, `size`) for each atlas glyph
    /// of a cell, given the cell's pen `origin_x`, text `baseline`, the advance
    /// box width `box_w` (one cell, or two for a double-width cell), and the cell
    /// height `cell_h`.
    ///
    /// The natural placement anchors a glyph at the single cell origin via its
    /// left/top bearings (`origin_x + left`, `baseline - top`). That is correct
    /// for a one-cell box. For a double-width box (`box_w == 2 * cell_w`) the
    /// glyph should instead sit centered across both cells:
    ///
    ///   * Mask glyphs (CJK text, monochrome emoji) keep their rasterized size and
    ///     their font bearings, plus a horizontal shift of `(box_w - cell_w) / 2`
    ///     that recenters the single-cell-anchored glyph in the two-cell box. For
    ///     a single-width cell that shift is zero, so ordinary text is unchanged.
    ///   * Color emoji are scaled to fit the box height (preserving aspect, capped
    ///     to the box width) and then centered both horizontally and vertically in
    ///     the box, so a square emoji bitmap fills its box without overflowing a
    ///     neighbour or clipping at the top/bottom.
    ///
    /// `cell_w` is the single-cell advance; `box_w` is this cell's advance box.
    /// `fg` is the glyph color. Pushes one [`FgInstance`] per atlas glyph directly
    /// into `out` (no per-cell temporary allocation — this runs for nearly every
    /// rebuilt cell, so the hot path stays alloc-free).
    #[allow(clippy::too_many_arguments)]
    fn place_glyphs(
        out: &mut Vec<FgInstance>,
        glyphs: &[AtlasGlyph],
        fg: [f32; 4],
        origin_x: f32,
        baseline: f32,
        cell_w: f32,
        box_w: f32,
        cell_h: f32,
    ) {
        // Horizontal recentering for a wide box (0 for a single-width cell).
        let center_dx = (box_w - cell_w) * 0.5;
        for g in glyphs {
            let (pos, size) = if g.is_color {
                // Color emoji: scale to fit the box height-first (preserving
                // aspect), capped to the box width, then center in the box.
                let scale = if g.px_h > 0.0 {
                    let s = cell_h / g.px_h;
                    if g.px_w * s > box_w && g.px_w > 0.0 {
                        box_w / g.px_w
                    } else {
                        s
                    }
                } else {
                    1.0
                };
                let w = g.px_w * scale;
                let h = g.px_h * scale;
                let x = origin_x + (box_w - w) * 0.5;
                let y = baseline - cell_h + (cell_h - h) * 0.5;
                ([x, y], [w, h])
            } else {
                // Mask glyph: keep its size and bearings; shift right to recenter
                // the single-cell-anchored glyph in the box.
                let x = origin_x + g.left as f32 + center_dx;
                let y = baseline - g.top as f32;
                ([x, y], [g.px_w, g.px_h])
            };
            out.push(FgInstance {
                pos,
                size,
                uv_min: g.uv_min,
                uv_max: g.uv_max,
                color: fg,
                flags: if g.is_color { 1 } else { 0 },
                _pad: [0; 3],
            });
        }
    }

    /// Queue a translucent overlay quad (panel body, dim backdrop, border rail) in
    /// PIXEL coordinates. `color` is straight RGBA; it is premultiplied here so the
    /// overlay pipeline (premultiplied blend) composites it over the terminal.
    /// Drawn after the grid + images, so it always lands on top.
    ///
    /// INVARIANT: callers (modal/menu paint) rely on the terminal grid under the
    /// panel having been freshly painted this frame. Every modal/menu open+close
    /// path sets `App::force_full_redraw`, which repaints all rows — so the terminal
    /// pixels are resident before these quads composite over them. New panel paths
    /// must keep that contract or the area under the glass may show stale content.
    pub fn push_overlay_px(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let a = color[3];
        self.overlay_quads.push(BgInstance {
            pos: [x, y],
            size: [w, h],
            color: [color[0] * a, color[1] * a, color[2] * a, a],
        });
    }

    /// Cell-rect convenience for overlay quads: covers `cols` x `rows` cells from
    /// cell origin (`col`,`row`). Matches `push_cell`'s pixel math.
    #[allow(dead_code)]
    pub fn push_overlay_cells(
        &mut self,
        col: usize,
        row: usize,
        cols: usize,
        rows: usize,
        color: [f32; 4],
    ) {
        let cw = self.metrics.width;
        let ch = self.metrics.height;
        let pad = self.pad;
        let x = col as f32 * cw + pad;
        let y = row as f32 * ch + pad;
        self.push_overlay_px(x, y, cols as f32 * cw, rows as f32 * ch, color);
    }

    /// Push a single panel glyph at cell (`col`,`row`) in color `fg`, into the
    /// text-on-glass channel so it draws AFTER the overlay quads (and stays crisp
    /// over the glass body). Mirrors `push_cell`'s ordinary glyph path; box/block
    /// procedural drawing and decorations are intentionally omitted (panel text is
    /// plain labels). No background quad is emitted (the glass shows through).
    #[allow(dead_code)]
    pub fn push_overlay_glyph(&mut self, col: usize, row: usize, ch: char, fg: [f32; 4]) {
        if ch == ' ' || ch == '\0' {
            return;
        }
        let cell_w = self.metrics.width;
        let box_w = cell_w;
        let cell_h = self.metrics.height;
        let pad = self.pad;
        let origin_x = col as f32 * cell_w + pad;
        let origin_y = row as f32 * cell_h + pad;
        let baseline = origin_y + self.metrics.ascent;
        self.ensure_glyphs(ch, false, false);
        if let Some(glyphs) = self.glyph_cache.get(&(ch, false, false)) {
            Self::place_glyphs(
                &mut self.overlay_text,
                glyphs,
                fg,
                origin_x,
                baseline,
                cell_w,
                box_w,
                cell_h,
            );
        }
    }

    /// Queue a rounded-rectangle overlay fill in PIXEL coordinates. Emits ONE
    /// [`FgInstance`] with `flags == 3` into the text-on-glass channel (so it
    /// composites above `push_overlay_px` quads, like overlay glyphs). The shader
    /// draws an antialiased SDF rounded rect; `radius` is the corner radius in px
    /// (clamped to the box in the shader, so 0 = sharp rect). `color` is straight
    /// RGBA; the shader premultiplies. This is the GUI layer's surface primitive.
    pub fn push_overlay_rrect_px(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: [f32; 4]) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        // Atlas UVs are unused for flags==3; we smuggle the radius through uv_min.x
        // (see vs_fg), and set uv to a clean 0..1 local coord via uv_min/uv_max.
        self.overlay_text.push(FgInstance {
            pos: [x, y],
            size: [w, h],
            uv_min: [radius, 0.0],
            uv_max: [radius, 1.0],
            color,
            flags: 3,
            _pad: [0; 3],
        });
    }

    /// Queue a rounded-rectangle overlay fill with INDEPENDENT per-corner radii.
    /// `radii` is (top-left, top-right, bottom-right, bottom-left) in px. Emits one
    /// `FgInstance` with `flags == 4`; the four radii are smuggled through
    /// `uv_min`/`uv_max` (the atlas UVs are unused on this path — see vs_fg). This
    /// lets the active tab round only its top corners while keeping its bottom edge
    /// square and flush to the content seam, so the connector patch no longer leaks
    /// background through the rrect corner feather. A single radius via
    /// [`push_overlay_rrect_px`] remains the common path.
    pub fn push_overlay_rrect4_px(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radii: [f32; 4],
        color: [f32; 4],
    ) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        self.overlay_text.push(FgInstance {
            pos: [x, y],
            size: [w, h],
            uv_min: [radii[0], radii[1]],
            uv_max: [radii[2], radii[3]],
            color,
            flags: 4,
            _pad: [0; 3],
        });
    }

    /// Begin capturing the tab-bar's overlay instances. Records the current overlay
    /// list lengths so [`Renderer::commit_tab_overlay`] can snapshot exactly the
    /// instances the tab-bar painter pushed. The tab bar is always painted first
    /// (before menus/settings/status), so its captured region is a clean prefix.
    pub fn begin_tab_overlay(&mut self) {
        self.tab_overlay_mark = Some((self.overlay_quads.len(), self.overlay_text.len()));
    }

    /// Finish capturing the tab-bar overlay region: copy the instances pushed since
    /// [`Renderer::begin_tab_overlay`] into the persistent tab-overlay cache, so a
    /// later unchanged frame can replay them without re-shaping tab titles.
    pub fn commit_tab_overlay(&mut self) {
        let Some((q0, t0)) = self.tab_overlay_mark.take() else {
            return;
        };
        self.tab_overlay_quads.clear();
        self.tab_overlay_text.clear();
        self.tab_overlay_quads
            .extend_from_slice(&self.overlay_quads[q0..]);
        self.tab_overlay_text
            .extend_from_slice(&self.overlay_text[t0..]);
    }

    /// Replay the cached tab-bar overlay instances into this frame's overlay lists,
    /// skipping the (glyph-shaping) tab-bar painter. Used when nothing tab-relevant
    /// changed since the last rebuild. A no-op if the cache is empty (the App falls
    /// back to a full rebuild in that case).
    pub fn replay_tab_overlay(&mut self) {
        self.overlay_quads.extend_from_slice(&self.tab_overlay_quads);
        self.overlay_text.extend_from_slice(&self.tab_overlay_text);
    }

    /// Whether a tab-bar overlay snapshot is cached and available for replay.
    pub fn has_tab_overlay(&self) -> bool {
        !self.tab_overlay_quads.is_empty() || !self.tab_overlay_text.is_empty()
    }

    /// Push a single panel glyph at an arbitrary PIXEL position (top-left of the
    /// glyph's cell box), in color `fg`, into the text-on-glass channel. This is
    /// the pixel-positioned counterpart of [`Renderer::push_overlay_glyph`] — it
    /// frees chrome text from the cell grid so the GUI layer can place labels at
    /// any sub-cell coordinate. No background quad is emitted.
    pub fn push_overlay_glyph_px(&mut self, x: f32, y: f32, ch: char, fg: [f32; 4]) {
        if ch == ' ' || ch == '\0' {
            return;
        }
        let cell_w = self.metrics.width;
        let cell_h = self.metrics.height;
        let baseline = y + self.metrics.ascent;
        self.ensure_glyphs(ch, false, false);
        if let Some(glyphs) = self.glyph_cache.get(&(ch, false, false)) {
            Self::place_glyphs(
                &mut self.overlay_text,
                glyphs,
                fg,
                x,
                baseline,
                cell_w,
                cell_w,
                cell_h,
            );
        }
    }

    /// Draw a whole string starting at PIXEL `(x, y)` (top-left of the first
    /// glyph's cell box), one monospace cell advance per char, in color `fg`.
    /// Convenience over [`push_overlay_glyph_px`] for GUI labels.
    pub fn push_overlay_glyph_px_str(&mut self, x: f32, y: f32, s: &str, fg: [f32; 4]) {
        let cw = self.metrics.width;
        let mut cx = x;
        for ch in s.chars() {
            self.push_overlay_glyph_px(cx, y, ch, fg);
            cx += cw;
        }
    }

    /// Width in physical px that `s` occupies when drawn with the panel glyph
    /// path. The font is monospace, so this is exact: one cell advance per char.
    /// Used by the GUI layer for centering / right-alignment of labels.
    pub fn text_width_px(&self, s: &str) -> f32 {
        s.chars().count() as f32 * self.metrics.width
    }

    /// Reserve `px` physical pixels above the terminal grid for the GUI tab bar.
    /// Pass 0 to restore the legacy (no-chrome) layout. Added to every grid cell's
    /// (and the cursor's) pixel origin in [`Renderer::push_cell`]/[`push_cursor`],
    /// so the single-pane terminal starts below the tab bar without any cell-row
    /// reservation. The multi-pane path leaves this at 0 (each pane carries its own
    /// pixel origin via `begin_pane`).
    pub fn set_grid_origin_y(&mut self, px: f32) {
        self.grid_origin_y = px.max(0.0);
    }

    /// The current grid top inset in physical px (see [`set_grid_origin_y`]).
    pub fn grid_origin_y(&self) -> f32 {
        self.grid_origin_y
    }

    /// Push a single solid-color rectangle as a [`BgInstance`]. Coordinates are
    /// physical pixels. Because the bg pass draws instances in insertion order
    /// with no depth test, a quad pushed here after a cell's background quad
    /// paints on top of it — that is how procedural box/block segments land in
    /// the foreground color over the cell background.
    fn push_solid(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        self.rows[self.cur_row].bg.push(BgInstance {
            pos: [x, y],
            size: [w, h],
            color,
        });
    }

    /// Paint a cursor overlay for the cell at `(col, row)` in the cursor color.
    /// Pushed via `push_solid` (bg pass) after every cell, so the bars/outline
    /// land on top of the cell background; glyphs still draw over them in the fg
    /// pass, keeping the character under the cursor legible.
    pub fn push_cursor(&mut self, col: usize, row: usize, overlay: CursorOverlay, color: [f32; 4]) {
        let cell_w = self.metrics.width;
        let cell_h = self.metrics.height;
        let ox = (col as f32 * cell_w + self.pad).round();
        let oy = (row as f32 * cell_h + self.pad + self.grid_origin_y).round();
        let w = cell_w.round();
        let h = cell_h.round();
        // Bar thickness for beam/underline and the outline rails.
        let th = (cell_h / 12.0).round().max(1.0);

        match overlay {
            CursorOverlay::Beam => self.push_solid(ox, oy, th, h, color),
            CursorOverlay::Underline => self.push_solid(ox, oy + h - th, w, th, color),
            CursorOverlay::Hollow => {
                self.push_solid(ox, oy, w, th, color); // top
                self.push_solid(ox, oy + h - th, w, th, color); // bottom
                self.push_solid(ox, oy, th, h, color); // left
                self.push_solid(ox + w - th, oy, th, h, color); // right
            }
        }
    }

    /// Paint a cell's underline + strikethrough strokes in the decoration color,
    /// using the font's recommended stroke positions and thickness from
    /// `CellMetrics`. Straight strokes are solid rectangles in the bg pass; the
    /// curly underline is a foreground decoration instance (procedural coverage).
    fn draw_decorations(&mut self, ox: f32, oy: f32, dec: Decorations) {
        if dec.underline == UnderlineStyle::None && !dec.strikeout {
            return;
        }
        let c = dec.color;
        let w = self.metrics.width;
        let th = self.metrics.decoration_thickness;
        let x = ox.round();
        let cell_h = self.metrics.height;

        if dec.strikeout {
            let y = (oy + self.metrics.strikeout_y).round();
            self.push_solid(x, y, w, th, c);
        }

        // Underline baseline y, clamped to stay inside the cell.
        let uy = ((oy + self.metrics.underline_y).round())
            .min(oy + cell_h - th)
            .max(oy);
        match dec.underline {
            UnderlineStyle::None => {}
            UnderlineStyle::Single => self.push_solid(x, uy, w, th, c),
            UnderlineStyle::Double => {
                // Two thin rails: the lower at the single-underline position, the
                // upper one stroke + gap above it, both kept inside the cell.
                let gap = th.max(1.0);
                let lower = uy;
                let upper = (lower - th - gap).max(oy);
                self.push_solid(x, upper, w, th, c);
                self.push_solid(x, lower, w, th, c);
            }
            UnderlineStyle::Dotted => {
                // ~2px dots with ~2px gaps along the underline row.
                let dot = th.max(1.0);
                let step = (dot * 2.0).max(2.0);
                let mut dx = x;
                while dx < x + w {
                    self.push_solid(dx, uy, dot, th, c);
                    dx += step;
                }
            }
            UnderlineStyle::Dashed => {
                // Three dashes per cell, ~70% on / 30% off, joining across cells.
                let slot = w / 3.0;
                let dash = (slot * 0.7).round().max(1.0);
                for i in 0..3 {
                    let dx = (x + slot * i as f32).round();
                    self.push_solid(dx, uy, dash, th, c);
                }
            }
            UnderlineStyle::Curl => {
                // A foreground decoration quad spanning the cell width, tall
                // enough for a visible wave; the fragment shader computes the
                // sine coverage. Center the band on the single-underline row.
                let band_h = (th * 3.0).max(4.0).min(cell_h - 1.0);
                let cy = uy + th * 0.5;
                let top = (cy - band_h * 0.5).max(oy).min(oy + cell_h - band_h);
                self.push_undercurl(x, top, w, band_h, c);
            }
        }
    }

    /// Push a foreground decoration instance for the curly underline. It carries
    /// no atlas glyph (uv is unused on the `flags == 2` path); the shader derives
    /// the wave from the quad's interpolated UV and pixel size.
    fn push_undercurl(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        // UV spans the full 0..1 quad so the shader can derive the local position
        // (and thus the sine wave) from the interpolated `uv` on the curl path.
        self.rows[self.cur_row].fg.push(FgInstance {
            pos: [x, y],
            size: [w, h],
            uv_min: [0.0, 0.0],
            uv_max: [1.0, 1.0],
            color,
            flags: 2,
            _pad: [0; 3],
        });
    }

    /// Paint a block element (U+2580..=U+259F) as exact solid foreground
    /// rectangles. All coordinates are rounded to whole pixels for crisp edges.
    ///
    /// `bg` is needed for the shade glyphs (U+2591..=U+2593): the background pass
    /// is created with `blend: None`, so an instance's alpha channel is written
    /// straight to the (opaque) surface and never composited. We therefore cannot
    /// express a shade as "fg at reduced alpha"; instead we pre-blend fg over bg
    /// by the shade coverage on the CPU and emit a fully-opaque solid color.
    fn draw_block(&mut self, ch: char, ox: f32, oy: f32, fg: [f32; 4], bg: [f32; 4]) {
        let cw = self.metrics.width;
        let chh = self.metrics.height;
        let l = ox.round();
        let t = oy.round();
        let r = (ox + cw).round();
        let b = (oy + chh).round();
        let w = r - l;
        let h = b - t;
        let cp = ch as u32;

        // Pre-blend fg over bg by `cov` coverage, producing an opaque color.
        // Used for the shade glyphs since the bg pass has blending disabled.
        fn shade(fg: [f32; 4], bg: [f32; 4], cov: f32) -> [f32; 4] {
            [
                bg[0] + (fg[0] - bg[0]) * cov,
                bg[1] + (fg[1] - bg[1]) * cov,
                bg[2] + (fg[2] - bg[2]) * cov,
                1.0,
            ]
        }

        // Helper closures expressed inline (no borrow of self) to compute the
        // fractional sub-rectangles, then pushed via push_solid.
        match cp {
            // Full block.
            0x2588 => self.push_solid(l, t, w, h, fg),
            // Upper half.
            0x2580 => {
                let mid = (oy + chh / 2.0).round();
                self.push_solid(l, t, w, mid - t, fg);
            }
            // Lower half.
            0x2584 => {
                let mid = (oy + chh / 2.0).round();
                self.push_solid(l, mid, w, b - mid, fg);
            }
            // Left half.
            0x258C => {
                let mid = (ox + cw / 2.0).round();
                self.push_solid(l, t, mid - l, h, fg);
            }
            // Right half.
            0x2590 => {
                let mid = (ox + cw / 2.0).round();
                self.push_solid(mid, t, r - mid, h, fg);
            }
            // Lower one-eighth through seven-eighths (U+2581..=U+2587).
            0x2581..=0x2587 => {
                let eighths = (cp - 0x2580) as f32; // 1..=7
                let top = (oy + chh * (1.0 - eighths / 8.0)).round();
                self.push_solid(l, top, w, b - top, fg);
            }
            // Left seven-eighths down to one-eighth (U+2589..=U+258F).
            0x2589..=0x258F => {
                // U+2589 = 7/8, U+258A = 6/8, ... U+258F = 1/8.
                let eighths = (8 - (cp - 0x2588)) as f32; // 7..=1
                let right = (ox + cw * (eighths / 8.0)).round();
                self.push_solid(l, t, right - l, h, fg);
            }
            // Light/medium/dark shades. The bg pass does not blend, so we mix fg
            // over bg by the shade coverage here and emit an opaque solid.
            0x2591 => self.push_solid(l, t, w, h, shade(fg, bg, 0.25)),
            0x2592 => self.push_solid(l, t, w, h, shade(fg, bg, 0.5)),
            0x2593 => self.push_solid(l, t, w, h, shade(fg, bg, 0.75)),
            // Quadrants (U+2596..=U+259F). Bit layout per quadrant:
            //   TL, TR, BL, BR. Each code point selects a subset.
            0x2596..=0x259F => {
                let mx = (ox + cw / 2.0).round();
                let my = (oy + chh / 2.0).round();
                let (tl, tr, bl, br) = match cp {
                    0x2596 => (false, false, true, false), // lower left
                    0x2597 => (false, false, false, true), // lower right
                    0x2598 => (true, false, false, false), // upper left
                    0x2599 => (true, false, true, true),   // UL+LL+LR
                    0x259A => (true, false, false, true),  // UL + LR
                    0x259B => (true, true, true, false),   // UL+UR+LL
                    0x259C => (true, true, false, true),   // UL+UR+LR
                    0x259D => (false, true, false, false), // upper right
                    0x259E => (false, true, true, false),  // UR + LL
                    0x259F => (false, true, true, true),   // UR+LL+LR
                    _ => (false, false, false, false),
                };
                if tl {
                    self.push_solid(l, t, mx - l, my - t, fg);
                }
                if tr {
                    self.push_solid(mx, t, r - mx, my - t, fg);
                }
                if bl {
                    self.push_solid(l, my, mx - l, b - my, fg);
                }
                if br {
                    self.push_solid(mx, my, r - mx, b - my, fg);
                }
            }
            // Any unhandled block code point: fill the cell so nothing is blank.
            _ => self.push_solid(l, t, w, h, fg),
        }
    }

    /// Paint a box-drawing character (U+2500..=U+257F) as solid foreground
    /// rectangles spanning the full cell so adjacent cells join seamlessly.
    /// Returns `true` if the code point was handled procedurally; `false` if the
    /// caller should fall back to the normal glyph path.
    fn draw_box(&mut self, ch: char, ox: f32, oy: f32, fg: [f32; 4]) -> bool {
        let cw = self.metrics.width;
        let chh = self.metrics.height;
        let thin = (chh / 14.0).round().max(1.0);
        let heavy = (thin * 2.0).round().max(2.0);
        // Center of the cell, rounded so the cross lands on whole pixels.
        let cx = (ox + cw / 2.0).round();
        let cy = (oy + chh / 2.0).round();
        // Cell edges (rounded so neighboring cells share an exact boundary).
        let left = ox.round();
        let right = (ox + cw).round();
        let top = oy.round();
        let bot = (oy + chh).round();

        // Arm weights: 0 = absent, 1 = light, 2 = heavy, 3 = double.
        const A: u8 = 0; // absent
        const L: u8 = 1; // light
        const H: u8 = 2; // heavy
        const D: u8 = 3; // double

        // Double rails sit symmetrically about the center line, each rail offset
        // by `rail` (= light thickness + 1px) from center, and each rail is
        // `thin` thick. So the near rail center is at `c - rail` and the far rail
        // center at `c + rail`. These coordinates are identical for horizontal
        // and vertical doubling, so straight doubles (═ ║) connect across cells.
        let rail = thin + 1.0;
        // Top edges of the two horizontal rails (centered on cy) and left edges
        // of the two vertical rails (centered on cx), rounded to whole pixels.
        let hy_near = (cy - rail - thin / 2.0).round(); // upper rail top
        let hy_far = (cy + rail - thin / 2.0).round(); // lower rail top
        let vx_near = (cx - rail - thin / 2.0).round(); // left rail left
        let vx_far = (cx + rail - thin / 2.0).round(); // right rail left
        // The outer extents of the double band: where the far rail of the
        // perpendicular axis ends. Used so double corners/junctions close.
        let h_outer_lo = hy_near; // top of the horizontal band
        let h_outer_hi = (hy_far + thin).round(); // bottom of the horizontal band
        let v_outer_lo = vx_near; // left of the vertical band
        let v_outer_hi = (vx_far + thin).round(); // right of the vertical band

        // Single-line (light/heavy) arm helpers. A light/heavy arm spans the
        // full half-cell from the edge to the center, centered on the cross, so
        // neighbours join. Heavy is identical layout, only thicker. Each closure
        // takes `this: &mut Self` explicitly so they don't borrow-conflict.
        let harm = |this: &mut Self, dir_left: bool, weight: u8| {
            if weight != L && weight != H {
                return;
            }
            let th = if weight == H { heavy } else { thin };
            let y = (cy - th / 2.0).round();
            if dir_left {
                this.push_solid(left, y, cx - left, th, fg);
            } else {
                this.push_solid(cx, y, right - cx, th, fg);
            }
        };
        let varm = |this: &mut Self, dir_up: bool, weight: u8| {
            if weight != L && weight != H {
                return;
            }
            let th = if weight == H { heavy } else { thin };
            let x = (cx - th / 2.0).round();
            if dir_up {
                this.push_solid(x, top, th, cy - top, fg);
            } else {
                this.push_solid(x, cy, th, bot - cy, fg);
            }
        };

        // Decode the code point into four arm weights (left, right, up, down).
        // `None` => not a simple-arm glyph; handled by the specials block below.
        let cp = ch as u32;
        let arms: Option<(u8, u8, u8, u8)> = match cp {
            // Straight lines.
            0x2500 => Some((L, L, A, A)), // light horizontal
            0x2501 => Some((H, H, A, A)), // heavy horizontal
            0x2502 => Some((A, A, L, L)), // light vertical
            0x2503 => Some((A, A, H, H)), // heavy vertical

            // Corners (light). U+250C down+right, etc.
            0x250C => Some((A, L, A, L)), // down and right
            0x250D => Some((A, H, A, L)),
            0x250E => Some((A, L, A, H)),
            0x250F => Some((A, H, A, H)), // heavy down and right
            0x2510 => Some((L, A, A, L)), // down and left
            0x2511 => Some((H, A, A, L)),
            0x2512 => Some((L, A, A, H)),
            0x2513 => Some((H, A, A, H)),
            0x2514 => Some((A, L, L, A)), // up and right
            0x2515 => Some((A, H, L, A)),
            0x2516 => Some((A, L, H, A)),
            0x2517 => Some((A, H, H, A)),
            0x2518 => Some((L, A, L, A)), // up and left
            0x2519 => Some((H, A, L, A)),
            0x251A => Some((L, A, H, A)),
            0x251B => Some((H, A, H, A)),

            // Vertical + right (T pointing right) U+251C..U+2523.
            0x251C => Some((A, L, L, L)),
            0x251D => Some((A, H, L, L)),
            0x251E => Some((A, L, H, L)),
            0x251F => Some((A, L, L, H)),
            0x2520 => Some((A, L, H, H)),
            0x2521 => Some((A, H, H, L)),
            0x2522 => Some((A, H, L, H)),
            0x2523 => Some((A, H, H, H)),

            // Vertical + left (T pointing left) U+2524..U+252B.
            0x2524 => Some((L, A, L, L)),
            0x2525 => Some((H, A, L, L)),
            0x2526 => Some((L, A, H, L)),
            0x2527 => Some((L, A, L, H)),
            0x2528 => Some((L, A, H, H)),
            0x2529 => Some((H, A, H, L)),
            0x252A => Some((H, A, L, H)),
            0x252B => Some((H, A, H, H)),

            // Horizontal + down (T pointing down) U+252C..U+2533.
            0x252C => Some((L, L, A, L)),
            0x252D => Some((H, L, A, L)),
            0x252E => Some((L, H, A, L)),
            0x252F => Some((H, H, A, L)),
            0x2530 => Some((L, L, A, H)),
            0x2531 => Some((H, L, A, H)),
            0x2532 => Some((L, H, A, H)),
            0x2533 => Some((H, H, A, H)),

            // Horizontal + up (T pointing up) U+2534..U+253B.
            0x2534 => Some((L, L, L, A)),
            0x2535 => Some((H, L, L, A)),
            0x2536 => Some((L, H, L, A)),
            0x2537 => Some((H, H, L, A)),
            0x2538 => Some((L, L, H, A)),
            0x2539 => Some((H, L, H, A)),
            0x253A => Some((L, H, H, A)),
            0x253B => Some((H, H, H, A)),

            // Crosses U+253C..U+254B.
            0x253C => Some((L, L, L, L)),
            0x253D => Some((H, L, L, L)),
            0x253E => Some((L, H, L, L)),
            0x253F => Some((H, H, L, L)),
            0x2540 => Some((L, L, H, L)),
            0x2541 => Some((L, L, L, H)),
            0x2542 => Some((L, L, H, H)),
            0x2543 => Some((H, L, H, L)),
            0x2544 => Some((L, H, H, L)),
            0x2545 => Some((H, L, L, H)),
            0x2546 => Some((L, H, L, H)),
            0x2547 => Some((H, H, H, L)),
            0x2548 => Some((H, H, L, H)),
            0x2549 => Some((H, L, H, H)),
            0x254A => Some((L, H, H, H)),
            0x254B => Some((H, H, H, H)),

            // Rounded corners — same arms as the sharp corners; we approximate
            // the curve with square joins (visually fine at terminal sizes).
            0x256D => Some((A, L, A, L)), // arc down and right
            0x256E => Some((L, A, A, L)), // arc down and left
            0x256F => Some((L, A, L, A)), // arc up and left
            0x2570 => Some((A, L, L, A)), // arc up and right

            // Half lines (single weight). U+2574 left, U+2575 up, U+2576 right,
            // U+2577 down (light); U+2578..U+257B the same, heavy.
            0x2574 => Some((L, A, A, A)),
            0x2575 => Some((A, A, L, A)),
            0x2576 => Some((A, L, A, A)),
            0x2577 => Some((A, A, A, L)),
            0x2578 => Some((H, A, A, A)),
            0x2579 => Some((A, A, H, A)),
            0x257A => Some((A, H, A, A)),
            0x257B => Some((A, A, A, H)),

            // Mixed-weight straight lines.
            0x257C => Some((L, H, A, A)), // left light, right heavy
            0x257D => Some((A, A, L, H)), // up light, down heavy
            0x257E => Some((H, L, A, A)), // left heavy, right light
            0x257F => Some((A, A, H, L)), // up heavy, down light

            _ => None,
        };

        if let Some((al, ar, au, ad)) = arms {
            harm(self, true, al);
            harm(self, false, ar);
            varm(self, true, au);
            varm(self, false, ad);
            return true;
        }

        // --- Doubles --------------------------------------------------------
        // Decode the double-line set (U+2550..U+256C) into the same four-arm
        // model, where each arm is Absent / Light (single) / Double. Every
        // double arm is rendered as two thin rails at the fixed `vx_*`/`hy_*`
        // offsets, so straight doubles connect across cells and corners close.
        let darms: Option<(u8, u8, u8, u8)> = match cp {
            0x2550 => Some((D, D, A, A)), // ═ double horizontal
            0x2551 => Some((A, A, D, D)), // ║ double vertical
            0x2552 => Some((A, D, A, L)), // ╒ right double, down single
            0x2553 => Some((A, L, A, D)), // ╓ right single, down double
            0x2554 => Some((A, D, A, D)), // ╔ double down and right
            0x2555 => Some((D, A, A, L)), // ╕ left double, down single
            0x2556 => Some((L, A, A, D)), // ╖ left single, down double
            0x2557 => Some((D, A, A, D)), // ╗ double down and left
            0x2558 => Some((A, D, L, A)), // ╘ right double, up single
            0x2559 => Some((A, L, D, A)), // ╙ right single, up double
            0x255A => Some((A, D, D, A)), // ╚ double up and right
            0x255B => Some((D, A, L, A)), // ╛ left double, up single
            0x255C => Some((L, A, D, A)), // ╜ left single, up double
            0x255D => Some((D, A, D, A)), // ╝ double up and left
            0x255E => Some((A, D, L, L)), // ╞ vertical single, right double
            0x255F => Some((A, L, D, D)), // ╟ vertical double, right single
            0x2560 => Some((A, D, D, D)), // ╠ vertical double, right double
            0x2561 => Some((D, A, L, L)), // ╡ vertical single, left double
            0x2562 => Some((L, A, D, D)), // ╢ vertical double, left single
            0x2563 => Some((D, A, D, D)), // ╣ vertical double, left double
            0x2564 => Some((D, D, A, L)), // ╤ horizontal double, down single
            0x2565 => Some((L, L, A, D)), // ╥ horizontal single, down double
            0x2566 => Some((D, D, A, D)), // ╦ double down and horizontal
            0x2567 => Some((D, D, L, A)), // ╧ horizontal double, up single
            0x2568 => Some((L, L, D, A)), // ╨ horizontal single, up double
            0x2569 => Some((D, D, D, A)), // ╩ double up and horizontal
            0x256A => Some((D, D, L, L)), // ╪ vertical single, horizontal double
            0x256B => Some((L, L, D, D)), // ╫ vertical double, horizontal single
            0x256C => Some((D, D, D, D)), // ╬ double vertical and horizontal
            _ => None,
        };

        if let Some((al, ar, au, ad)) = darms {
            let h_double = al == D || ar == D;
            let v_double = au == D || ad == D;
            // Inner edges of the perpendicular band's rails (used to mitre).
            let vx_near_in = (vx_near + thin).round(); // right edge of left rail
            let hy_near_in = (hy_near + thin).round(); // bottom edge of top rail
            // The four pure double corners are drawn explicitly below as clean
            // outer/inner L-joins; skip the generic rail spans for them so the
            // inner notch stays open (the canonical ╔ ╗ ╚ ╝ look).
            let pure_corner = matches!(cp, 0x2554 | 0x2557 | 0x255A | 0x255D);

            // Each doubled axis is rendered as two parallel `thin` rails at the
            // fixed `hy_*`/`vx_*` offsets. The rail ENDPOINTS toward the center
            // are mitred so the band corners close: an "outer" rail wraps around
            // to its perpendicular outer rail, the "inner" rail makes the small
            // inner corner. Endpoints are chosen so straight doubles span the
            // full cell (connecting across cells) and corners/junctions close.

            // --- Horizontal rails (upper = hy_near, lower = hy_far). ---
            // Upper rail x-extent.
            let up_lo = if al == D {
                left
            } else if v_double {
                // No left arm: upper rail starts at the left vertical rail. It is
                // the outer rail for an up-and-right opening (╚/╠ etc.), inner for
                // a down-and-right opening (╔). Meet the near (left) vertical rail.
                vx_near
            } else {
                cx
            };
            let up_hi = if ar == D {
                right
            } else if v_double {
                vx_near_in
            } else {
                cx
            };
            // Lower rail x-extent.
            let lo_lo = if al == D {
                left
            } else if v_double {
                vx_near
            } else {
                cx
            };
            let lo_hi = if ar == D {
                right
            } else if v_double {
                vx_near_in
            } else {
                cx
            };
            if h_double && !pure_corner {
                self.push_solid(up_lo, hy_near, up_hi - up_lo, thin, fg);
                self.push_solid(lo_lo, hy_far, lo_hi - lo_lo, thin, fg);
            }

            // --- Vertical rails (left = vx_near, right = vx_far). ---
            let lf_lo = if au == D {
                top
            } else if h_double {
                hy_near
            } else {
                cy
            };
            let lf_hi = if ad == D {
                bot
            } else if h_double {
                hy_near_in
            } else {
                cy
            };
            let rt_lo = if au == D {
                top
            } else if h_double {
                hy_near
            } else {
                cy
            };
            let rt_hi = if ad == D {
                bot
            } else if h_double {
                hy_near_in
            } else {
                cy
            };
            if v_double && !pure_corner {
                self.push_solid(vx_near, lf_lo, thin, lf_hi - lf_lo, fg);
                self.push_solid(vx_far, rt_lo, thin, rt_hi - rt_lo, fg);
            }

            // --- Pure double corners: redraw the two rails as clean L-joins so
            // the outer/inner mitre is exact (overrides the generic spans above
            // only where it improves the join; the extra solids are harmless). ---
            match cp {
                0x2554 => {
                    // ╔ down+right: outer = top rail + left vrail; inner = bottom
                    // rail + right vrail.
                    self.push_solid(vx_near, hy_near, right - vx_near, thin, fg);
                    self.push_solid(vx_near, hy_near, thin, bot - hy_near, fg);
                    self.push_solid(vx_far, hy_far, right - vx_far, thin, fg);
                    self.push_solid(vx_far, hy_far, thin, bot - hy_far, fg);
                }
                0x2557 => {
                    // ╗ down+left.
                    self.push_solid(left, hy_near, vx_far + thin - left, thin, fg);
                    self.push_solid(vx_far, hy_near, thin, bot - hy_near, fg);
                    self.push_solid(left, hy_far, vx_near + thin - left, thin, fg);
                    self.push_solid(vx_near, hy_far, thin, bot - hy_far, fg);
                }
                0x255A => {
                    // ╚ up+right.
                    self.push_solid(vx_near, hy_far, right - vx_near, thin, fg);
                    self.push_solid(vx_near, top, thin, hy_far + thin - top, fg);
                    self.push_solid(vx_far, hy_near, right - vx_far, thin, fg);
                    self.push_solid(vx_far, top, thin, hy_near + thin - top, fg);
                }
                0x255D => {
                    // ╝ up+left.
                    self.push_solid(left, hy_far, vx_far + thin - left, thin, fg);
                    self.push_solid(vx_far, top, thin, hy_far + thin - top, fg);
                    self.push_solid(left, hy_near, vx_near + thin - left, thin, fg);
                    self.push_solid(vx_near, top, thin, hy_near + thin - top, fg);
                }
                _ => {}
            }

            // --- Single (light) arms crossing a doubled perpendicular band.
            // Draw a centered thin line that bridges the band so the single arm
            // connects to neighbours. ---
            if al == L {
                let y = (cy - thin / 2.0).round();
                let end = if v_double { v_outer_hi } else { cx };
                self.push_solid(left, y, end - left, thin, fg);
            }
            if ar == L {
                let y = (cy - thin / 2.0).round();
                let start = if v_double { v_outer_lo } else { cx };
                self.push_solid(start, y, right - start, thin, fg);
            }
            if au == L {
                let x = (cx - thin / 2.0).round();
                let end = if h_double { h_outer_hi } else { cy };
                self.push_solid(x, top, thin, end - top, fg);
            }
            if ad == L {
                let x = (cx - thin / 2.0).round();
                let start = if h_double { h_outer_lo } else { cy };
                self.push_solid(x, start, thin, bot - start, fg);
            }
            return true;
        }

        // --- Specials -------------------------------------------------------
        match cp {
            // Dashed horizontals: light/heavy double/triple/quadruple dash.
            // U+2504/2505 triple-dash, U+2508/2509 quadruple-dash (horizontal).
            0x2504 | 0x2508 => {
                self.draw_dashed_h(ox, cy, fg, thin, if cp == 0x2504 { 3 } else { 4 });
                true
            }
            0x2505 | 0x2509 => {
                self.draw_dashed_h(ox, cy, fg, heavy, if cp == 0x2505 { 3 } else { 4 });
                true
            }
            // U+2506/2507 triple-dash, U+250A/250B quadruple-dash (vertical).
            0x2506 | 0x250A => {
                self.draw_dashed_v(oy, cx, fg, thin, if cp == 0x2506 { 3 } else { 4 });
                true
            }
            0x2507 | 0x250B => {
                self.draw_dashed_v(oy, cx, fg, heavy, if cp == 0x2507 { 3 } else { 4 });
                true
            }
            // Diagonals U+2571..U+2573: hard to do well with axis-aligned
            // rectangles; fall back to the glyph path.
            0x2571..=0x2573 => false,
            // Anything else in the box range we don't explicitly handle: fall
            // back to the glyph so it never renders blank.
            _ => false,
        }
    }

    /// Draw a dashed horizontal line of the given thickness centered on `cy`,
    /// broken into `segments` dashes with gaps.
    fn draw_dashed_h(&mut self, ox: f32, cy: f32, fg: [f32; 4], th: f32, segments: u32) {
        let cw = self.metrics.width;
        let left = ox.round();
        let y = (cy - th / 2.0).round();
        let n = segments as f32;
        // Dash occupies ~70% of each slot, gap the rest.
        let slot = cw / n;
        let dash = (slot * 0.7).round().max(1.0);
        for i in 0..segments {
            let x = (left + slot * i as f32).round();
            self.push_solid(x, y, dash, th, fg);
        }
    }

    /// Draw a dashed vertical line of the given thickness centered on `cx`.
    fn draw_dashed_v(&mut self, oy: f32, cx: f32, fg: [f32; 4], th: f32, segments: u32) {
        let chh = self.metrics.height;
        let top = oy.round();
        let x = (cx - th / 2.0).round();
        let n = segments as f32;
        let slot = chh / n;
        let dash = (slot * 0.7).round().max(1.0);
        for i in 0..segments {
            let y = (top + slot * i as f32).round();
            self.push_solid(x, y, th, dash, fg);
        }
    }

    pub fn render(&mut self) -> RenderResult {
        // wgpu 29 returns a `CurrentSurfaceTexture` enum (there is no `SurfaceError`
        // in this version). `Success`/`Suboptimal` give us a frame; the remaining
        // variants are transient acquisition failures. We self-heal `Lost`/`Outdated`
        // by reconfiguring with the stored size and skip the frame; other states are
        // skipped silently and retried next redraw.
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

        // Flatten changed rows + upload only the dirty sub-ranges (or grow + full
        // upload when a row's count shifted the layout).
        self.end_frame();
        let bg_count = self.bg_count;
        let fg_count = self.fg_count;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });
        self.record_passes(&view, &mut encoder, bg_count, fg_count);

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }

    /// Record the bg + fg instanced draws into `view`. Shared by the on-screen
    /// `render()` path and the offscreen `capture()` path.
    fn record_passes(
        &self,
        view: &wgpu::TextureView,
        encoder: &mut wgpu::CommandEncoder,
        bg_count: u32,
        fg_count: u32,
    ) {
        let [r, g, b, a] = self.clear_color;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("grid-pass"),
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

        if bg_count > 0 {
            pass.set_pipeline(&self.bg_pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_vertex_buffer(0, self.unit_quad.slice(..));
            pass.set_vertex_buffer(1, self.bg_buffer.slice(..));
            pass.draw(0..4, 0..bg_count);
        }
        if fg_count > 0 {
            pass.set_pipeline(&self.fg_pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_bind_group(1, &self.atlas_bind_group, &[]);
            pass.set_vertex_buffer(0, self.unit_quad.slice(..));
            pass.set_vertex_buffer(1, self.fg_buffer.slice(..));
            pass.draw(0..4, 0..fg_count);
        }
        // Inline images overlay on top of the grid, reusing the fg pipeline with
        // the image atlas bound in the color slot (image quads use flags == 1).
        // The fg pass above already bound the fg pipeline, uniform group, and the
        // unit-quad vertex buffer identically, so when it ran we only need to swap
        // the atlas bind group + instance buffer here.
        if self.image_count > 0 {
            if fg_count == 0 {
                pass.set_pipeline(&self.fg_pipeline);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, self.unit_quad.slice(..));
            }
            pass.set_bind_group(1, &self.image_bind_group, &[]);
            pass.set_vertex_buffer(1, self.image_buffer.slice(..));
            pass.draw(0..4, 0..self.image_count);
        }
        // Translucent panel overlay (modals / menus): drawn last so it composites
        // over the finished grid + images. Body / backdrop / border rails use the
        // bg vertex layout with the premultiplied-blend overlay pipeline.
        if self.overlay_count > 0 {
            pass.set_pipeline(&self.overlay_pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_vertex_buffer(0, self.unit_quad.slice(..));
            pass.set_vertex_buffer(1, self.overlay_buffer.slice(..));
            pass.draw(0..4, 0..self.overlay_count);
        }
        // Panel text-on-glass: glyphs drawn after the overlay quads so they stay
        // crisp on top of the translucent body. Reuses the fg pipeline + text atlas.
        if self.overlay_text_count > 0 {
            pass.set_pipeline(&self.fg_pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_bind_group(1, &self.atlas_bind_group, &[]);
            pass.set_vertex_buffer(0, self.unit_quad.slice(..));
            pass.set_vertex_buffer(1, self.overlay_text_buffer.slice(..));
            pass.draw(0..4, 0..self.overlay_text_count);
        }
    }

    // --- Multi-pane (split) render path ------------------------------------
    //
    // The single-grid path above (begin_frame/begin_row/push_cell/render) is the
    // zero-overhead default used whenever a tab has one pane. The methods below
    // are a SEPARATE path for tiled splits: each pane is built relative to its
    // own pixel origin and drawn under its own scissor rect, so panes never bleed
    // into one another. This path forgoes per-row damage tracking (it rebuilds
    // every frame) — splitting is rare, and the simplicity is worth it.
    //
    // Usage per frame:
    //   begin_multi_frame(default_bg);
    //   for each pane:
    //     begin_pane(rect, focused);
    //     begin_row(r); push_cell(...); ...        // local (col,row) as usual
    //     push_cursor(...);                         // local (col,row)
    //     end_pane();
    //   draw_divider(rect, ...) / focus border emitted by end_pane;
    //   render_multi();
    //
    // `begin_row`/`push_cell`/`push_cursor` are reused verbatim: a pane authors
    // its cells into `self.rows` exactly like the single-grid path, and `end_pane`
    // translates those instances by the pane's pixel origin into the flat
    // per-pane lists. So all the box-drawing / decoration / glyph / cursor logic
    // is shared with zero duplication.

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
                cached.bg.push(BgInstance { pos: [x, y], size: [th, h], color: c }); // left
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
            scissor: clamp_scissor(0, 0, self.config.width as i32, self.config.height as i32, self.config.width, self.config.height),
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
    fn upload_mp_buffer<T: bytemuck::Pod>(
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
    fn record_multi_passes(&self, view: &wgpu::TextureView, encoder: &mut wgpu::CommandEncoder) {
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

    /// Render the current frame to an offscreen texture and write it to `path`
    /// as a binary PPM (P6). Used for headless screenshot verification.
    pub fn capture(&mut self, path: &std::path::Path) -> Result<()> {
        self.end_frame();
        let bg_count = self.bg_count;
        let fg_count = self.fg_count;
        self.capture_with(path, |s, view, enc| s.record_passes(view, enc, bg_count, fg_count))
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
        self.capture_with(path, |s, view, enc| s.record_multi_passes(view, enc))
    }

    /// Shared offscreen-capture machinery: allocate a render target, let `record`
    /// emit its passes into it, copy it back, and write a binary PPM (P6).
    fn capture_with(
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
    fn end_frame(&mut self) {
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
            self.queue
                .write_buffer(&self.image_buffer, 0, bytemuck::cast_slice(&self.image_overlay));
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
            self.queue
                .write_buffer(&self.overlay_buffer, 0, bytemuck::cast_slice(&self.overlay_quads));
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
            self.queue
                .write_buffer(&self.overlay_text_buffer, 0, bytemuck::cast_slice(&self.overlay_text));
        }
        self.dirty_rows.clear();
    }

    /// Upload the overlay quad + text-on-glass buffers (rebuilt every frame) and
    /// set their draw counts. Shared by the single-pane `end_frame` path and the
    /// multi-pane `render_multi`/`capture_multi` paths so GUI chrome composites in
    /// both. Idempotent; safe to call when the lists are empty.
    fn upload_overlay_buffers(&mut self) {
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
            self.queue
                .write_buffer(&self.overlay_buffer, 0, bytemuck::cast_slice(&self.overlay_quads));
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
            self.queue
                .write_buffer(&self.overlay_text_buffer, 0, bytemuck::cast_slice(&self.overlay_text));
        }
    }

    /// Upload a single instance pass (bg or fg), returning the total instance
    /// count for the draw call. See [`Renderer::end_frame`] for the strategy. The
    /// `pick` closure selects the per-row vector for the pass.
    #[allow(clippy::too_many_arguments)]
    fn flush_pass<T: bytemuck::Pod>(
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

    /// Ensure the glyph(s) for `(ch, bold, italic)` are rasterized and packed
    /// into the atlas, recording their `AtlasGlyph` entries in the cache.
    fn ensure_glyphs(&mut self, ch: char, bold: bool, italic: bool) {
        let key = (ch, bold, italic);
        if self.glyph_cache.contains_key(&key) {
            return;
        }
        let rasters = collect_rasters(&self.text.rasterize(ch, bold, italic));
        let packed = self.pack_rasters(&rasters);
        self.glyph_cache.insert(key, packed);
    }

    /// Like `ensure_glyphs`, but for a full grapheme cluster (combining/ZWJ
    /// sequence) shaped as a single unit.
    fn ensure_cluster_glyphs(&mut self, cluster: &str, bold: bool, italic: bool) {
        let key = (cluster.to_string(), bold, italic);
        if self.cluster_cache.contains_key(&key) {
            return;
        }
        let rasters = collect_rasters(&self.text.rasterize_cluster(cluster, bold, italic));
        let packed = self.pack_rasters(&rasters);
        self.cluster_cache.insert(key, packed);
    }

    /// Pack owned glyph bitmaps into the atlases, returning their placed entries.
    /// Coverage-mask glyphs go into the R8 mask atlas; color glyphs (emoji) go
    /// into the RGBA8 color atlas. If either atlas fills mid-pack, both glyph
    /// caches and both packers are cleared and we repack once (entries are
    /// re-created lazily on demand thereafter).
    fn pack_rasters(&mut self, rasters: &[Raster]) -> Vec<AtlasGlyph> {
        let mut packed: Vec<AtlasGlyph> = Vec::with_capacity(rasters.len());
        let mut retried = false;
        let inv_mask = 1.0 / ATLAS_SIZE as f32;
        let inv_color = 1.0 / COLOR_ATLAS_SIZE as f32;
        'attempt: loop {
            packed.clear();
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
                        if retried {
                            log::warn!("glyph atlas full; a glyph was skipped");
                            break 'attempt;
                        }
                        log::warn!("glyph atlas full; clearing cache and repacking");
                        self.glyph_cache.clear();
                        self.cluster_cache.clear();
                        self.packer.reset();
                        self.color_packer.reset();
                        // Every cached glyph's atlas position just changed, so any
                        // row already built this frame (and persisted rows from
                        // earlier frames) now hold stale UVs. Flag a full rebuild.
                        self.atlas_reset = true;
                        retried = true;
                        continue 'attempt;
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
            break;
        }
        packed
    }
}

#[cfg(test)]
mod tests {
    use super::clamp_scissor;

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
}
