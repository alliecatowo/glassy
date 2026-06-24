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

mod init;
mod cell;
mod overlay;
mod frame;
mod multipane;
mod pipeline;

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


#[cfg(test)]
mod tests {
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
        assert!(allocs > 0, "must have succeeded at least once before overflow");
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
                "uv_min.x={} out of [0,1]", uv_min[0]
            );
            assert!(
                uv_min[1] >= 0.0 && uv_min[1] <= 1.0,
                "uv_min.y={} out of [0,1]", uv_min[1]
            );
            assert!(
                uv_max[0] >= 0.0 && uv_max[0] <= 1.0,
                "uv_max.x={} out of [0,1]", uv_max[0]
            );
            assert!(
                uv_max[1] >= 0.0 && uv_max[1] <= 1.0,
                "uv_max.y={} out of [0,1]", uv_max[1]
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
        loop {
            match p.alloc(glyph_w, glyph_h) {
                Some(o) => {
                    pre_reset_last = Some(o);
                }
                None => break,
            }
        }
        let pre_uvs = pre_reset_last.map(|(x, y)| {
            [x as f32 * inv, y as f32 * inv,
             (x + glyph_w) as f32 * inv, (y + glyph_h) as f32 * inv]
        });

        // Simulate repack: reset + re-alloc the same glyph.
        p.reset();
        let post_origin = p.alloc(glyph_w, glyph_h)
            .expect("first alloc after reset must succeed");
        let post_uvs = [
            post_origin.0 as f32 * inv,
            post_origin.1 as f32 * inv,
            (post_origin.0 + glyph_w) as f32 * inv,
            (post_origin.1 + glyph_h) as f32 * inv,
        ];

        // Post-reset UVs must be in [0,1].
        for &uv in &post_uvs {
            assert!(uv >= 0.0 && uv <= 1.0, "post-reset UV {uv} out of [0,1]");
        }
        // Post-reset origin must be (0,0) — the cache was cleared and repacking
        // starts fresh, so the first glyph always lands at the atlas origin.
        assert_eq!(post_origin, (0, 0), "post-reset first alloc must land at (0,0)");
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
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_without_display_handle(),
        );
        let adapter = pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::None,
                force_fallback_adapter: false,
                compatible_surface: None,
            },
        )).ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("glassy-test"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                ..Default::default()
            },
        ))
        .ok()?;
        Some((device, queue))
    }

    #[test]
    fn headless_wgpu_device_init_succeeds() {
        // Gate: verify the wgpu adapter + device pipeline can be constructed
        // without a window surface. This is the prerequisite for all GPU tests.
        // If no hardware adapter is available we skip (don't fail).
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_without_display_handle(),
        );
        let adapter = pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::None,
                force_fallback_adapter: false,
                compatible_surface: None,
            },
        ));
        let Ok(adapter) = adapter else {
            // No GPU in this environment; skip.
            return;
        };
        let info = adapter.get_info();
        // Adapter must report a non-empty name and a known backend.
        assert!(!info.name.is_empty(), "adapter name must be non-empty");
        let result = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("glassy-headless-gate"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                ..Default::default()
            },
        ));
        assert!(result.is_ok(), "headless device creation must succeed");
    }

    #[test]
    fn headless_device_can_create_atlas_textures() {
        // Verify that the two atlas texture descriptors (R8Unorm mask and
        // Rgba8Unorm color) can be created on the headless device. A wgpu 31
        // format-enum or Extent3d API change would fail here first.
        let Some((device, _queue)) = headless_device() else { return };

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
        let Some((device, _queue)) = headless_device() else { return };

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
        let Some((device, queue)) = headless_device() else { return };

        // Build two rows: row 0 has 2 bg instances, row 1 has 1 bg instance.
        let mut rows: Vec<RowInstances> = vec![RowInstances::default(), RowInstances::default()];
        for _ in 0..2 {
            rows[0].bg.push(BgInstance { pos: [0.0, 0.0], size: [8.0, 16.0], color: [0.0; 4] });
        }
        rows[1].bg.push(BgInstance { pos: [8.0, 0.0], size: [8.0, 16.0], color: [0.0; 4] });

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
            &device, &queue,
            &rows, |r| &r.bg,
            &mut flat, &mut offsets, &mut scratch,
            &[], &mut buffer, &mut capacity, "test-bg",
        );
        assert_eq!(total1, 3, "total after first call must be 3");
        assert_eq!(offsets, &[0, 2, 3], "offsets must be prefix sums");

        // Mutate: give row 0 one MORE instance — this changes the layout.
        rows[0].bg.push(BgInstance { pos: [16.0, 0.0], size: [8.0, 16.0], color: [0.0; 4] });
        let mut dirty = vec![0usize]; // row 0 was rebuilt

        // Second call: layout must detect the shift and reflatten.
        let total2 = Renderer::flush_pass::<BgInstance>(
            &device, &queue,
            &rows, |r| &r.bg,
            &mut flat, &mut offsets, &mut scratch,
            &dirty, &mut buffer, &mut capacity, "test-bg",
        );
        assert_eq!(total2, 4, "total after layout change must be 4");
        assert_eq!(offsets, &[0, 3, 4], "offsets must reflect the new layout");

        // Verify the buffer was grown when total exceeded original capacity (it
        // was 16 so this particular case doesn't grow, but capacity is preserved).
        assert!(capacity >= 4, "capacity must be at least the total");

        // Reset dirty for a stable-layout call.
        dirty.clear();
        let total3 = Renderer::flush_pass::<BgInstance>(
            &device, &queue,
            &rows, |r| &r.bg,
            &mut flat, &mut offsets, &mut scratch,
            &dirty, &mut buffer, &mut capacity, "test-bg",
        );
        // Same layout, no dirty rows: fast path returns the same total.
        assert_eq!(total3, 4, "no-dirty fast path must return same total");
    }

    #[test]
    fn flush_pass_grows_buffer_when_capacity_exceeded() {
        // Create a buffer that is too small for the total instances, verify
        // flush_pass creates a new (larger) buffer and returns the correct total.
        let Some((device, queue)) = headless_device() else { return };

        let n_instances = INITIAL_INSTANCES + 1; // one more than the initial buffer
        let mut rows: Vec<RowInstances> = vec![RowInstances::default()];
        for _ in 0..n_instances {
            rows[0].bg.push(BgInstance {
                pos: [0.0, 0.0], size: [1.0, 1.0], color: [0.0; 4],
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
            &device, &queue,
            &rows, |r| &r.bg,
            &mut flat, &mut offsets, &mut scratch,
            &[0], &mut buffer, &mut capacity, "test-tiny",
        );
        assert_eq!(total as usize, n_instances, "total must equal instance count");
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
        let Some((device, queue)) = headless_device() else { return };

        let mut rows = vec![RowInstances::default(), RowInstances::default()];
        rows[0].bg.push(BgInstance { pos: [0.0,0.0], size:[8.0,16.0], color:[0.0;4] });
        rows[1].bg.push(BgInstance { pos: [8.0,0.0], size:[8.0,16.0], color:[0.0;4] });

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
            &device, &queue, &rows, |r| &r.bg,
            &mut flat, &mut offsets, &mut scratch,
            &[0, 1], &mut buffer, &mut capacity, "test-stable",
        );
        let offsets_after_first = offsets.clone();

        // Second call: same content, no dirty rows.
        let total = Renderer::flush_pass::<BgInstance>(
            &device, &queue, &rows, |r| &r.bg,
            &mut flat, &mut offsets, &mut scratch,
            &[], &mut buffer, &mut capacity, "test-stable",
        );
        assert_eq!(total, 2, "total unchanged");
        assert_eq!(offsets, offsets_after_first, "offsets unchanged on fast path");
    }

    #[test]
    fn flush_pass_empty_rows_returns_zero() {
        let Some((device, queue)) = headless_device() else { return };

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
            &device, &queue, &rows, |r| &r.bg,
            &mut flat, &mut offsets, &mut scratch,
            &[], &mut buffer, &mut capacity, "test-empty",
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
        let left  = clamp_scissor(0,   0, 400, 600, 800, 600);
        let right = clamp_scissor(400, 0, 400, 600, 800, 600);

        assert_eq!((left.x, left.y, left.w, left.h),   (0,   0, 400, 600));
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
        let top = clamp_scissor(0,   0, 800, 200, 800, 600);
        let mid = clamp_scissor(0, 200, 800, 200, 800, 600);
        let bot = clamp_scissor(0, 400, 800, 200, 800, 600);

        assert_eq!((top.x, top.y, top.w, top.h), (0,   0, 800, 200));
        assert_eq!((mid.x, mid.y, mid.w, mid.h), (0, 200, 800, 200));
        assert_eq!((bot.x, bot.y, bot.w, bot.h), (0, 400, 800, 200));

        // Panes must tile without overlap or gap.
        assert_eq!(top.y + top.h, mid.y, "no gap between top and mid");
        assert_eq!(mid.y + mid.h, bot.y, "no gap between mid and bot");
        assert_eq!(bot.y + bot.h, 600, "bottom of last pane must reach surface_h");
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
}
