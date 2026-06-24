//! Font loading, cell metrics, and on-demand glyph rasterization.
//!
//! This module is deliberately free of any GPU/windowing dependency: it shapes
//! single characters with `cosmic-text` and rasterizes them to RGBA8 bitmaps via
//! the bundled `swash` cache. The renderer uploads the resulting bitmaps into a
//! glyph atlas; everything here is pure CPU work and is fully cached per
//! `(char, bold, italic)` so repeated cells are a cheap `HashMap` lookup.

#[cfg(unix)]
use std::collections::HashSet;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use std::sync::Arc;

/// Path to the on-disk fc-match resolution cache.
///
/// Layout: one entry per line, tab-separated `pattern\tfile_path`. The cache
/// prevents repeat `fc-match` subprocess invocations on subsequent glassy
/// launches (the "fc-match storm" at startup). Invalid / stale entries are
/// ignored — a failed lookup just falls through to a live `fc-match` call and
/// refreshes the cache entry.
#[cfg(unix)]
fn fc_cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".cache"))
        })?;
    Some(base.join("glassy/fc-cache.tsv"))
}

/// Load the entire fc-match cache into a `HashMap<pattern, file_path>`.
/// Silently returns an empty map on any I/O or parse error.
#[cfg(unix)]
fn fc_cache_load() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let path = match fc_cache_path() {
        Some(p) => p,
        None => return map,
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return map,
    };
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('\t')
            && !k.is_empty() && !v.is_empty() {
                map.insert(k.to_string(), v.to_string());
            }
    }
    map
}

/// Persist a single `pattern → file_path` mapping into the fc-match cache.
/// Creates the parent directory if absent. Errors are logged at debug level
/// and do not abort the font load.
#[cfg(unix)]
fn fc_cache_insert(pattern: &str, file_path: &str) {
    let path = match fc_cache_path() {
        Some(p) => p,
        None => return,
    };
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent) {
            log::debug!("glassy: fc-cache dir create failed: {e}");
            return;
        }
    // Append-only: one line per entry. The cache grows monotonically; a stale
    // entry is harmless because lookup also validates the path still exists.
    use std::io::Write;
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{pattern}\t{file_path}") {
                log::debug!("glassy: fc-cache write failed: {e}");
            }
        }
        Err(e) => log::debug!("glassy: fc-cache open failed: {e}"),
    }
}

use anyhow::Result;
use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight,
    fontdb,
};

/// Per-cell layout metrics, all in physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    pub ascent: f32,
    /// Y of the underline stroke's top edge, measured DOWN from the cell top
    /// (so the renderer can draw it directly at `origin_y + underline_y`).
    pub underline_y: f32,
    /// Y of the strikethrough stroke's top edge, measured down from the cell top.
    pub strikeout_y: f32,
    /// Thickness of decoration strokes (underline / strikeout), in pixels.
    pub decoration_thickness: f32,
}

/// A single rasterized glyph bitmap plus its placement relative to the pen.
pub struct RasterizedGlyph {
    /// Bitmap size in pixels (both 0 for blank cells with nothing to draw).
    pub width: u32,
    pub height: u32,
    /// Horizontal offset from the pen origin to the bitmap's left edge.
    pub left: i32,
    /// Vertical offset from the baseline UP to the bitmap's top edge
    /// (positive = above the baseline), per swash's placement convention.
    pub top: i32,
    /// `true` when `data` is the glyph's own color (e.g. emoji); `false` when it
    /// is a single-channel coverage mask.
    pub is_color: bool,
    /// Glyph pixels. For a coverage mask (`is_color == false`) this is one byte
    /// per pixel (R8 coverage), length == `width * height`. For a color glyph
    /// (`is_color == true`) it is RGBA8, length == `width * height * 4`. Empty
    /// when either dimension is 0.
    pub data: Vec<u8>,
}

/// Owns the shaping/rasterization state. Rasterized glyphs are *not* cached here;
/// the renderer caches the packed atlas glyphs and only calls in on a miss.
pub struct Text {
    font_system: FontSystem,
    swash_cache: SwashCache,
    /// Reused for every shaping call to avoid reallocating line buffers.
    buffer: Buffer,
    /// The resolved font family. We store the name as an owned `String` because
    /// `Family::Name` borrows; `attrs()` rebuilds the borrowing `Family` per call.
    family: FamilyOwned,
}

/// Owned counterpart to `cosmic_text::Family`, holding the name string so we can
/// hand out a freshly-borrowed `Family<'_>` whenever we build `Attrs`.
enum FamilyOwned {
    Monospace,
    Named(String),
}

impl FamilyOwned {
    fn as_family(&self) -> Family<'_> {
        match self {
            FamilyOwned::Monospace => Family::Monospace,
            FamilyOwned::Named(name) => Family::Name(name),
        }
    }
}

/// Build shaping attributes for the given style against the resolved family.
fn build_attrs<'a>(family: Family<'a>, bold: bool, italic: bool) -> Attrs<'a> {
    let mut attrs = Attrs::new();
    attrs.family = family;
    attrs.weight = if bold { Weight::BOLD } else { Weight::NORMAL };
    attrs.style = if italic { Style::Italic } else { Style::Normal };
    attrs
}

/// A font candidate produced by discovery: the raw file bytes, the resolved file
/// path it came from (when it originated from a concrete file, used to de-dup the
/// primary against the fallback chain), and a short label describing its origin
/// (used only for logging/diagnostics).
struct FontCandidate {
    bytes: Vec<u8>,
    /// Absolute file path the bytes were read from, if known. `None` only when a
    /// candidate's bytes did not come from a single on-disk file.
    path: Option<PathBuf>,
    source_label: String,
}

/// The outcome of building a `FontSystem` from a single candidate's bytes: the
/// constructed system, the owned family to shape with, and whether the face we
/// loaded actually reports itself as monospaced.
struct LoadedFont {
    font_system: FontSystem,
    family: FamilyOwned,
    is_monospaced: bool,
}

impl Text {
    /// Discover a monospace font, load it, and measure the cell box for `font_px`.
    ///
    /// `family` is an optional preferred family name (from config/CLI). When set
    /// it is tried first (resolved via fontconfig, verified to actually be that
    /// family); discovery then falls back to the curated list and the rest of the
    /// chain so a typo'd or absent family still yields a usable monospace font.
    pub fn load(family: Option<&str>, font_px: f32) -> Result<(Text, CellMetrics)> {
        // Gather candidate *producers* in priority order (explicit override,
        // requested family, curated families verified via fontconfig, generic
        // monospace, known paths). Each producer is a closure that only runs its
        // (potentially expensive: an `fc-match` subprocess + a font read) work
        // when actually polled — so once an early candidate loads as a usable
        // monospace face we stop and never pay for the rest of the chain. This is
        // a large startup win: the common case (FiraCode present) used to run an
        // `fc-match` for every curated family before picking the first one.
        let producers = discover_font_producers(family);

        // Build a `FontSystem` from the first producer that yields a usable
        // monospaced face. A non-monospaced face is accepted only as a last
        // resort (no producer after it yields a usable face).
        let mut loaded: Option<LoadedFont> = None;
        let mut fallback: Option<LoadedFont> = None;
        for producer in producers {
            let Some(candidate) = producer() else {
                continue;
            };
            match build_font_system(candidate.bytes, candidate.path) {
                Some(found) => {
                    if found.is_monospaced {
                        loaded = Some(found);
                        break;
                    }
                    log::debug!(
                        "glassy: candidate '{}' is not monospaced; keeping as fallback",
                        candidate.source_label
                    );
                    fallback.get_or_insert(found);
                }
                None => {
                    log::debug!(
                        "glassy: candidate '{}' had no usable face, trying next",
                        candidate.source_label
                    );
                }
            }
        }
        let loaded = loaded.or(fallback);

        // If nothing loaded, fall back to a full system font scan.
        // This is intentionally a last resort — it walks every font on the
        // system and is O(hundreds of files) on a typical Linux install, so
        // it adds hundreds of milliseconds to startup. Warn so the user (or
        // packager) knows the curated discovery chain failed entirely.
        let (font_system, family) = match loaded {
            Some(found) => (found.font_system, found.family),
            None => {
                log::warn!(
                    "glassy: no usable monospace font found via fc-match or probe paths; \
                     falling back to full system font scan (slow). \
                     Install a monospace font (e.g. JetBrains Mono, DejaVu Sans Mono) \
                     or set GLASSY_FONT=<path> to suppress this."
                );
                (FontSystem::new(), FamilyOwned::Monospace)
            }
        };

        // Line height is the cell height; round so cells land on pixel
        // boundaries. 1.30x the em gives comfortable vertical spacing.
        let line_height = (font_px * 1.30).round();
        let metrics = Metrics::new(font_px, line_height);

        let mut text = Text {
            font_system,
            swash_cache: SwashCache::new(),
            buffer: Buffer::new_empty(metrics),
            family,
        };

        let cell = text.measure_cell(font_px, line_height);
        let family_name = match &text.family {
            FamilyOwned::Named(n) => n.as_str(),
            FamilyOwned::Monospace => "<system monospace>",
        };
        log::info!(
            "font='{}' font_px={:.1} cell={}x{} ascent={}",
            family_name,
            font_px,
            cell.width,
            cell.height,
            cell.ascent
        );
        Ok((text, cell))
    }

    /// Shape a representative string to derive the monospace advance width and
    /// the baseline (ascent) for one line, plus the decoration (underline /
    /// strikeout) stroke positions read from the primary font's metrics.
    fn measure_cell(&mut self, font_px: f32, line_height: f32) -> CellMetrics {
        let attrs = build_attrs(self.family.as_family(), false, false);
        self.buffer.set_text("MM", &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);

        let mut cell_w = font_fallback_width(line_height);
        let mut ascent = line_height.round();
        // The font id of the shaped face, so we can read its decoration metrics.
        let mut font_id = None;

        if let Some(run) = self.buffer.layout_runs().next() {
            ascent = run.line_y.round();
            // Two identical glyphs let us read the pen advance directly as the
            // gap between their origins; for a single glyph fall back to its width.
            if run.glyphs.len() >= 2 {
                cell_w = (run.glyphs[1].x - run.glyphs[0].x).round().max(1.0);
            } else if let Some(g) = run.glyphs.first() {
                cell_w = g.w.ceil().max(1.0);
            }
            font_id = run.glyphs.first().map(|g| (g.font_id, g.font_weight));
        }

        let (underline_y, strikeout_y, decoration_thickness) =
            self.decoration_metrics(font_id, font_px, ascent, line_height);

        CellMetrics {
            width: cell_w,
            height: line_height,
            ascent,
            underline_y,
            strikeout_y,
            decoration_thickness,
        }
    }

    /// Read the underline/strikeout stroke positions and thickness from the
    /// shaped face's swash metrics, falling back to sensible em-relative values
    /// when the font reports nothing usable.
    ///
    /// swash reports `underline_offset` / `strikeout_offset` as the distance
    /// from the baseline UP to the top of the stroke (positive = above the
    /// baseline). Underlines sit below the baseline, so that offset is normally
    /// negative; we convert to a top-of-cell-relative Y with `ascent - offset`.
    /// Returned Ys are the stroke's top edge, measured down from the cell top.
    fn decoration_metrics(
        &mut self,
        font_id: Option<(fontdb::ID, Weight)>,
        font_px: f32,
        ascent: f32,
        line_height: f32,
    ) -> (f32, f32, f32) {
        // Em-relative fallbacks used when the font lacks usable metrics.
        let fallback_thickness = (line_height / 16.0).round().max(1.0);
        let fallback_underline = (ascent + fallback_thickness * 2.0).round();
        let fallback_strikeout = (ascent - (ascent * 0.30)).round();

        let Some((id, weight)) = font_id else {
            return (fallback_underline, fallback_strikeout, fallback_thickness);
        };
        let Some(font) = self.font_system.get_font(id, weight) else {
            return (fallback_underline, fallback_strikeout, fallback_thickness);
        };

        // `metrics(&[])` yields the unscaled (font-unit) metrics; `scale(px)`
        // converts to pixels for our em size (`font_px`).
        let m = font.as_swash().metrics(&[]).scale(font_px);

        let thickness = if m.stroke_size > 0.0 {
            m.stroke_size.round().max(1.0)
        } else {
            fallback_thickness
        };

        // Underline: top edge = ascent - underline_offset (offset is negative).
        let underline_y = if m.underline_offset != 0.0 {
            (ascent - m.underline_offset).round()
        } else {
            fallback_underline
        };
        // Strikeout: offset is positive (above baseline). Its value is the
        // CENTER/top per the OS/2 table; we treat it as the stroke top.
        let strikeout_y = if m.strikeout_offset != 0.0 {
            (ascent - m.strikeout_offset).round()
        } else {
            fallback_strikeout
        };

        // Clamp the underline so it stays inside the cell (some faces report an
        // offset that would push the stroke past the descender region).
        let max_y = (line_height - thickness).max(0.0);
        let underline_y = underline_y.min(max_y).max(0.0);
        let strikeout_y = strikeout_y.min(max_y).max(0.0);

        (underline_y, strikeout_y, thickness)
    }

    /// Return the glyph bitmap(s) needed to draw `ch` in the given style.
    ///
    /// Blank cells (spaces and anything with no drawable coverage) yield an empty
    /// Vec. Not cached here: the renderer caches the *packed atlas glyphs* and only
    /// calls this on a cache miss, so a second Text-level cache would just retain a
    /// duplicate bitmap that is never read.
    pub fn rasterize(&mut self, ch: char, bold: bool, italic: bool) -> Vec<RasterizedGlyph> {
        let mut tmp = [0u8; 4];
        self.build_glyphs(ch.encode_utf8(&mut tmp), bold, italic)
    }

    /// Rasterize a full grapheme cluster (a base character plus its combining
    /// marks / ZWJ-joined codepoints) as a single shaped unit, so compound
    /// emoji (flags, ZWJ sequences, skin-tone modifiers) and combining
    /// sequences resolve to their single combined glyph. Used only for cells that
    /// actually carry combiners; the common single-character path stays on
    /// `rasterize`. Uncached for the same reason as [`Text::rasterize`].
    pub fn rasterize_cluster(&mut self, cluster: &str, bold: bool, italic: bool) -> Vec<RasterizedGlyph> {
        self.build_glyphs(cluster, bold, italic)
    }

    /// Shape and rasterize a single character into owned RGBA bitmaps.
    fn build_glyphs(&mut self, text: &str, bold: bool, italic: bool) -> Vec<RasterizedGlyph> {
        // `family` borrows `self`, so capture the borrowed `Family` before the
        // `&mut self.font_system` borrows below; they touch disjoint fields.
        let attrs = build_attrs(self.family.as_family(), bold, italic);
        self.buffer.set_text(text, &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);

        let mut out = Vec::new();
        for run in self.buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let pg = glyph.physical((0.0, 0.0), 1.0);
                // Clone the Option so the FontSystem borrow ends before we read
                // the image and build the glyph below.
                let img_opt = self
                    .swash_cache
                    .get_image(&mut self.font_system, pg.cache_key)
                    .clone();
                let Some(img) = img_opt else { continue };

                let (w, h) = (img.placement.width, img.placement.height);
                if w == 0 || h == 0 {
                    continue;
                }
                let pixels = (w * h) as usize;

                let (data, is_color) = match img.content {
                    // 1 byte/px coverage: keep it as-is for the R8 mask atlas.
                    SwashContent::Mask => (img.data.clone(), false),
                    // 3 bytes/px subpixel coverage; collapse to a single coverage
                    // byte for the R8 mask atlas.
                    SwashContent::SubpixelMask => {
                        let mut d = Vec::with_capacity(pixels);
                        for px in img.data.chunks_exact(3) {
                            d.push(px[0].max(px[1]).max(px[2]));
                        }
                        (d, false)
                    }
                    // Already RGBA (color emoji): keep as-is for the color atlas.
                    SwashContent::Color => (img.data.clone(), true),
                };

                out.push(RasterizedGlyph {
                    width: w,
                    height: h,
                    left: img.placement.left,
                    top: img.placement.top,
                    is_color,
                    data,
                });
            }
        }
        out
    }
}

/// Best-effort fallback advance when shaping yields no measurable glyph.
fn font_fallback_width(line_height: f32) -> f32 {
    (line_height * 0.5).round().max(1.0)
}

/// Build a `FontSystem` from a single primary font's raw bytes, then enrich the
/// same database with a small fallback chain so cosmic-text's per-glyph fallback
/// can resolve code points the primary font lacks (CJK, emoji, misc symbols).
///
/// Crucially, the *primary* face is the first source loaded, and it is the only
/// font we point the generic monospace family at and shape with (`Family::Named`
/// of its family), so ASCII/Latin always shapes with the primary font. The
/// fallback fonts are merely additional sources in the same `fontdb::Database`;
/// because we shape with `Shaping::Advanced`, cosmic-text walks the database for
/// faces covering missing glyphs and renders them instead of tofu.
///
/// `primary_path` is the file the primary bytes were read from, if known; it
/// seeds the de-dup set so the fallback chain never reloads the primary file.
///
/// We deliberately avoid `FontSystem::new()` (a full system scan) here — only a
/// handful of fontconfig-resolved fallback files are loaded.
///
/// Returns `None` if the primary bytes contained no usable face.
fn build_font_system(bytes: Vec<u8>, primary_path: Option<PathBuf>) -> Option<LoadedFont> {
    let mut db = fontdb::Database::new();
    let ids = db.load_font_source(fontdb::Source::Binary(Arc::new(bytes)));

    // The first face among the ids we just loaded is *our* face.
    let face = ids.iter().filter_map(|id| db.face(*id)).next();
    let family_name = face.and_then(|f| f.families.first().map(|(n, _)| n.clone()));
    let is_monospaced = face.map(|f| f.monospaced).unwrap_or(false);

    let family_name = family_name?;

    // Map the generic `monospace` family onto our font as well, so any fallback
    // path through `Family::Monospace` still resolves to the font we loaded.
    db.set_monospace_family(family_name.clone());

    // Load the fc-match resolution cache once; both style and fallback loading
    // benefit from it (cache hits skip the subprocess entirely).
    #[cfg(unix)]
    let fc_cache = fc_cache_load();

    // Load the bold/italic faces of the same family so styled text shapes with
    // the real monospace face rather than falling back to a proportional font.
    #[cfg(unix)]
    load_primary_styles(&mut db, &family_name, primary_path.as_deref(), &fc_cache);

    // Enrich the database with fallback faces (best-effort; failures are skipped).
    #[cfg(unix)]
    load_fallback_fonts(&mut db, primary_path.as_deref(), &fc_cache);
    #[cfg(not(unix))]
    let _ = primary_path;

    let font_system = FontSystem::new_with_locale_and_db("en-US".to_string(), db);
    Some(LoadedFont {
        font_system,
        family: FamilyOwned::Named(family_name),
        is_monospaced,
    })
}

/// Fallback families to resolve via fontconfig and add to the database, in order.
/// Each entry is an `fc-match` pattern; we resolve it to a concrete file with
/// `fc-match -f %{file} "<pattern>"`. Multiple patterns cover the same script so
/// that whichever the host actually has installed gets pulled in.
// Emoji is handled separately (see `load_emoji_fallback`): we load a bundled
// CBDT color-bitmap face by path, because swash cannot rasterize the COLRv1
// "Noto Color Emoji" that fontconfig resolves to on most hosts.
#[cfg(unix)]
const FALLBACK_PATTERNS: &[&str] = &[
    // CJK coverage.
    "Noto Sans CJK",
    "sans-serif:lang=ja",
    // Miscellaneous symbols.
    "Noto Sans Symbols2",
    "sans-serif",
];

/// Resolve the fallback patterns via fontconfig and load each distinct file into
/// `db`. De-duplicates by resolved file path and never reloads the primary file.
///
/// The resolution phase (fc-match subprocesses) is parallelized via
/// `thread::scope` — all patterns are resolved concurrently, then the results
/// are loaded serially into `db`. Resolved paths are written to the fc-cache
/// so subsequent launches skip the subprocesses entirely.
#[cfg(unix)]
fn load_fallback_fonts(
    db: &mut fontdb::Database,
    primary_path: Option<&Path>,
    cache: &std::collections::HashMap<String, String>,
) {
    // Seed the seen set with the primary file (canonicalized when possible) so we
    // never load it a second time as a fallback.
    let mut seen: HashSet<PathBuf> = HashSet::new();
    if let Some(p) = primary_path {
        seen.insert(canonical_or_owned(p));
    }

    load_emoji_fallback(db, &mut seen, cache);

    // Resolve all fallback patterns in parallel — each fc-match is a subprocess
    // round-trip (~5–30 ms each on a cold fontconfig cache); doing them
    // concurrently shaves ~100 ms off cold startups.
    //
    // Strategy: for each pattern, check the cache (free); if a miss, spawn a
    // scoped thread for the fc-match subprocess. Collect handles inside the
    // scope, then join them (also inside the scope) to get `Vec<(pattern, path)>`.
    // `thread::scope` blocks until all threads finish, so the result is ready
    // when the closure returns.
    let resolved: Vec<(&str, Option<String>)> = std::thread::scope(|s| {
        // Phase 1: for each pattern, either return a cache hit or a join handle.
        enum Resolution<'scope, 'env> {
            Cached(Option<String>),
            Spawned(std::thread::ScopedJoinHandle<'scope, Option<String>>, std::marker::PhantomData<&'env ()>),
        }
        let work: Vec<(&str, Resolution<'_, '_>)> = FALLBACK_PATTERNS
            .iter()
            .map(|pattern| {
                if let Some(cached_path) = cache.get(*pattern)
                    && Path::new(cached_path).exists() {
                        return (*pattern, Resolution::Cached(Some(cached_path.clone())));
                    }
                let handle = s.spawn(move || fc_match_file_live(pattern));
                (*pattern, Resolution::Spawned(handle, std::marker::PhantomData))
            })
            .collect();
        // Phase 2: join all handles (cache hits pass through directly).
        work.into_iter()
            .map(|(pat, res)| match res {
                Resolution::Cached(path) => (pat, path),
                Resolution::Spawned(handle, _) => (pat, handle.join().unwrap_or(None)),
            })
            .collect()
    });

    for (pattern, path_opt) in resolved {
        let Some(path) = path_opt else { continue };
        // Persist to cache if it was a live lookup (not already in cache).
        if !cache.contains_key(pattern) {
            fc_cache_insert(pattern, &path);
        }
        let key = canonical_or_owned(Path::new(&path));
        if !seen.insert(key) {
            continue;
        }
        if load_font_file(db, &path) {
            log::debug!("glassy: loaded fallback font for '{pattern}': {path}");
        } else {
            log::debug!("glassy: fallback '{pattern}' resolved to unreadable {path}");
        }
    }
}

/// Load the bold, italic, and bold-italic faces of the primary `family` into
/// `db`, so styled text shapes with the real (monospace) face instead of
/// falling back to a proportional font for those styles. Best-effort: a style
/// that fontconfig resolves back to the already-loaded regular file (e.g. a
/// font with no italic, like FiraCode) is de-duplicated and skipped.
///
/// The three style lookups are resolved in parallel via `thread::scope`, then
/// loaded serially. New mappings are written to the fc-cache.
#[cfg(unix)]
fn load_primary_styles(
    db: &mut fontdb::Database,
    family: &str,
    primary_path: Option<&Path>,
    cache: &std::collections::HashMap<String, String>,
) {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    if let Some(p) = primary_path {
        seen.insert(canonical_or_owned(p));
    }
    let patterns = [
        format!("{family}:weight=bold"),
        format!("{family}:slant=italic"),
        format!("{family}:weight=bold:slant=italic"),
    ];

    // Resolve all three style patterns concurrently.
    let resolved: Vec<(String, Option<String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = patterns
            .iter()
            .map(|pattern| {
                // Cache hit: no thread needed.
                if let Some(cached_path) = cache.get(pattern)
                    && Path::new(cached_path).exists() {
                        return (pattern.clone(), Ok(Some(cached_path.clone())));
                    }
                let pattern_clone = pattern.clone();
                let handle = s.spawn(move || fc_match_file_live(&pattern_clone));
                (pattern.clone(), Err(handle))
            })
            .collect();
        handles
            .into_iter()
            .map(|(pat, result)| match result {
                Ok(path) => (pat, path),
                Err(handle) => (pat, handle.join().unwrap_or(None)),
            })
            .collect()
    });

    for (pattern, path_opt) in resolved {
        let Some(path) = path_opt else { continue };
        if !cache.contains_key(&pattern) {
            fc_cache_insert(&pattern, &path);
        }
        let key = canonical_or_owned(Path::new(&path));
        if !seen.insert(key) {
            continue;
        }
        if load_font_file(db, &path) {
            log::debug!("glassy: loaded style face '{pattern}': {path}");
        }
    }
}

/// Load the emoji fallback face.
///
/// We prefer a bundled **CBDT color-bitmap** Noto Color Emoji (loaded by an
/// explicit path), because swash can rasterize CBDT/sbix bitmaps into full-color
/// glyphs — whereas the COLRv1 "Noto Color Emoji" that fontconfig resolves to on
/// most modern hosts is unrenderable by swash and comes out blank. Only if no
/// bundled color face is present do we fall back to a monochrome emoji face.
#[cfg(unix)]
fn load_emoji_fallback(
    db: &mut fontdb::Database,
    seen: &mut HashSet<PathBuf>,
    cache: &std::collections::HashMap<String, String>,
) {
    if let Some(path) = color_emoji_path() {
        let key = canonical_or_owned(&path);
        if seen.insert(key) {
            // The bundled color emoji face is ~11 MB; load it memory-mapped so the
            // bytes are only paged in if a session actually renders an emoji.
            if load_font_file(db, &path) {
                log::debug!("glassy: loaded color emoji: {}", path.display());
                return;
            }
        }
    }

    // No bundled color emoji: fall back to a monochrome face (drawn in the fg
    // color). `:color=false` forces fontconfig away from an unrenderable COLRv1
    // face toward the monochrome NotoEmoji outline font.
    for pattern in ["Noto Emoji:color=false", "emoji"] {
        if let Some(path) = fc_match_file_cached(pattern, cache) {
            if !cache.contains_key(pattern) {
                fc_cache_insert(pattern, &path);
            }
            let key = canonical_or_owned(Path::new(&path));
            if seen.insert(key) && load_font_file(db, &path) {
                log::debug!("glassy: loaded monochrome emoji for '{pattern}': {path}");
                return;
            }
        }
    }
}

/// Locate the bundled CBDT color emoji font, searching the XDG data dir.
#[cfg(unix)]
fn color_emoji_path() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(xdg));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share"));
    }
    roots
        .into_iter()
        .map(|r| r.join("glassy/fonts/NotoColorEmoji.ttf"))
        .find(|p| p.is_file())
}

/// Canonicalize a path for de-dup purposes, falling back to the path as-is when
/// canonicalization fails (e.g. the file is gone between resolve and read).
#[cfg(unix)]
fn canonical_or_owned(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Resolve an arbitrary `fc-match` pattern to a single concrete file path.
///
/// Unlike `fc_match_family`, we do *not* verify the resolved family — these are
/// fallback fonts, so whatever file fontconfig returns for the pattern is
/// acceptable (fontconfig always returns *some* installed file).
///
/// `cache` is the pre-loaded fc-cache map; a hit skips the subprocess entirely
/// (the path is re-validated with `Path::exists` to catch stale entries).
#[cfg(unix)]
fn fc_match_file_cached(
    pattern: &str,
    cache: &std::collections::HashMap<String, String>,
) -> Option<String> {
    // Check the disk cache first — a valid hit avoids the subprocess.
    if let Some(cached_path) = cache.get(pattern)
        && Path::new(cached_path).exists() {
            log::debug!("glassy: fc-cache hit for '{pattern}': {cached_path}");
            return Some(cached_path.clone());
        }
    fc_match_file_live(pattern)
}

/// Run a live `fc-match` subprocess (no cache involved).
#[cfg(unix)]
fn fc_match_file_live(pattern: &str) -> Option<String> {
    let output = Command::new("fc-match")
        .args(["-f", "%{file}", pattern])
        .output()
        .map_err(|err| log::debug!("glassy: fc-match unavailable: {err}"))
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}

/// A lazy font-candidate producer: invoking it runs its discovery work (which may
/// spawn an `fc-match` subprocess and read a font file) and yields the candidate,
/// or `None` if that source is absent/unreadable. Boxed so the staged chain can
/// be a single `Vec` regardless of each stage's capture.
type CandidateProducer = Box<dyn FnOnce() -> Option<FontCandidate>>;

/// Build the ordered chain of lazy candidate *producers*. Returning closures
/// (rather than eagerly materializing every candidate) lets [`Text::load`] stop
/// at the first producer that yields a usable monospace face, so a host with a
/// good default font never pays the `fc-match` + read cost of the rest of the
/// chain. Order: explicit override, requested family, curated families, generic
/// monospace, then known install paths.
fn discover_font_producers(requested: Option<&str>) -> Vec<CandidateProducer> {
    let mut producers: Vec<CandidateProducer> = Vec::new();

    // Load the fc-match resolution cache once upfront. Curated-family closures
    // capture a clone; a cache hit in the closure avoids the subprocess entirely.
    #[cfg(unix)]
    let fc_cache = fc_cache_load();

    // 1. Explicit override: an absolute path to a font file.
    producers.push(Box::new(|| {
        let path = std::env::var("GLASSY_FONT").ok()?;
        let bytes = read_font(&path)?;
        Some(FontCandidate {
            bytes,
            path: Some(PathBuf::from(&path)),
            source_label: format!("GLASSY_FONT={path}"),
        })
    }));

    // 1b. Config/CLI-requested family: resolve via fontconfig and verify it is
    //     genuinely that family (fc-match returns a fallback otherwise). An
    //     absolute path is also accepted directly as a font file.
    #[cfg(unix)]
    if let Some(name) = requested {
        let name = name.trim().to_string();
        if !name.is_empty() {
            let cache_clone = fc_cache.clone();
            producers.push(Box::new(move || {
                // Allow `font_family` to be an explicit file path.
                let as_path = Path::new(&name);
                if as_path.is_file() {
                    let bytes = read_font(&name)?;
                    return Some(FontCandidate {
                        bytes,
                        path: Some(PathBuf::from(&name)),
                        source_label: format!("font_family path ({name})"),
                    });
                }
                if let Some(path) = fc_match_family_cached(&name, &cache_clone) {
                    if !cache_clone.contains_key(&format!("family:{name}")) {
                        fc_cache_insert(&format!("family:{name}"), &path);
                    }
                    let bytes = read_font(&path)?;
                    return Some(FontCandidate {
                        bytes,
                        path: Some(PathBuf::from(&path)),
                        source_label: format!("font_family {name} ({path})"),
                    });
                }
                log::warn!("glassy: requested font_family '{name}' not found; using default");
                None
            }));
        }
    }
    #[cfg(not(unix))]
    let _ = requested;

    // 2. A curated list of good monospace families, each resolved to a concrete
    //    file via fontconfig and verified to actually *be* that family (fc-match
    //    returns a nearest fallback even when the family is absent). One producer
    //    per family so discovery stops at the first installed one.
    #[cfg(unix)]
    for family in CURATED_FAMILIES {
        let cache_clone = fc_cache.clone();
        producers.push(Box::new(move || {
            let path = fc_match_family_cached(family, &cache_clone)?;
            if !cache_clone.contains_key(&format!("family:{family}")) {
                fc_cache_insert(&format!("family:{family}"), &path);
            }
            let bytes = read_font(&path)?;
            Some(FontCandidate {
                bytes,
                path: Some(PathBuf::from(&path)),
                source_label: format!("{family} ({path})"),
            })
        }));
    }

    // 3. Generic monospace via fontconfig; always a real monospace face.
    #[cfg(unix)]
    {
        let cache_clone = fc_cache.clone();
        producers.push(Box::new(move || {
            let path = fc_match_monospace_cached(&cache_clone)?;
            if !cache_clone.contains_key("monospace") {
                fc_cache_insert("monospace", &path);
            }
            let bytes = read_font(&path)?;
            Some(FontCandidate {
                bytes,
                path: Some(PathBuf::from(&path)),
                source_label: format!("fc-match monospace ({path})"),
            })
        }));
    }

    // 4. Probe well-known install locations as a last resort.
    for path in PROBE_PATHS {
        producers.push(Box::new(move || {
            let bytes = read_font(path)?;
            Some(FontCandidate {
                bytes,
                path: Some(PathBuf::from(path)),
                source_label: format!("probe ({path})"),
            })
        }));
    }

    producers
}

/// Load a font file into `db` by path (memory-mapped via fontdb), so the face
/// bytes are not copied onto the heap and are only paged in on demand when a
/// glyph from that face is rasterized. Returns `true` on success. Used for the
/// fallback/style chain, where most faces (CJK, emoji, symbols) are never
/// touched in an ordinary ASCII session and should not cost idle memory.
#[cfg(unix)]
fn load_font_file(db: &mut fontdb::Database, path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    match db.load_font_file(path) {
        Ok(()) => true,
        Err(err) => {
            log::debug!("glassy: skipping font {}: {err}", path.display());
            false
        }
    }
}

/// Read a font file, logging and skipping on any I/O error. Paths may contain
/// `[`/`]` (variable fonts, e.g. `NotoSansMono[wght].ttf`); `std::fs::read`
/// treats the path verbatim, so no glob/escaping handling is needed.
fn read_font(path: impl AsRef<Path>) -> Option<Vec<u8>> {
    let path = path.as_ref();
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            log::debug!("glassy: skipping font {}: {err}", path.display());
            None
        }
    }
}

/// Curated, high-quality monospace families to try first, in priority order.
/// `FiraCode Nerd Font Mono` is the ideal default when present.
#[cfg(unix)]
const CURATED_FAMILIES: &[&str] = &[
    "FiraCode Nerd Font Mono",
    "JetBrains Mono",
    "JetBrainsMono Nerd Font",
    "Cascadia Code",
    "Hack",
    "Iosevka",
    "DejaVu Sans Mono",
    "Liberation Mono",
];

/// Query fontconfig for a specific family, returning its file path only if the
/// match is genuinely that family. `fc-match` always returns *some* font (a
/// nearest fallback), so we must confirm the resolved family name contains the
/// requested family (case-insensitive) before trusting the file.
///
/// `cache` is the pre-loaded fc-cache map; a hit skips the subprocess (path
/// is re-validated with `Path::exists` to catch stale entries).
#[cfg(unix)]
fn fc_match_family_cached(
    family: &str,
    cache: &std::collections::HashMap<String, String>,
) -> Option<String> {
    // For family lookups we store the key as "family:<name>" to avoid
    // collisions with bare `fc_match_file` pattern keys.
    let key = format!("family:{family}");
    if let Some(cached_path) = cache.get(&key)
        && Path::new(cached_path).exists() {
            log::debug!("glassy: fc-cache hit for family '{family}': {cached_path}");
            return Some(cached_path.clone());
        }
    fc_match_family_live(family)
}

/// Run a live `fc-match` family lookup (no cache involved).
#[cfg(unix)]
fn fc_match_family_live(family: &str) -> Option<String> {
    let output = Command::new("fc-match")
        .args(["-f", "%{family}\t%{file}", family])
        .output()
        .map_err(|err| log::debug!("glassy: fc-match unavailable: {err}"))
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout);
    let (matched_family, file) = line.split_once('\t')?;
    let file = file.trim();
    if file.is_empty() {
        return None;
    }
    // `%{family}` may be a comma-separated list of alias names; accept the file
    // if any of them contains the requested family name, case-insensitively.
    let wanted = family.to_lowercase();
    let is_match = matched_family
        .split(',')
        .any(|name| name.trim().to_lowercase().contains(&wanted));
    if is_match {
        Some(file.to_string())
    } else {
        log::debug!(
            "glassy: fc-match for '{family}' returned fallback '{}', skipping",
            matched_family.trim()
        );
        None
    }
}

/// Query fontconfig for the resolved monospace font file path.
#[cfg(unix)]
fn fc_match_monospace_cached(
    cache: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let key = "monospace";
    if let Some(cached_path) = cache.get(key)
        && Path::new(cached_path).exists() {
            log::debug!("glassy: fc-cache hit for monospace: {cached_path}");
            return Some(cached_path.clone());
        }
    let output = Command::new("fc-match")
        .args(["-f", "%{file}", "monospace"])
        .output()
        .map_err(|err| log::debug!("glassy: fc-match unavailable: {err}"))
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}


/// Known monospace font locations, probed in order as a last resort.
#[cfg(target_os = "macos")]
const PROBE_PATHS: &[&str] = &[
    "/System/Library/Fonts/SFNSMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Monaco.ttf",
    "/Library/Fonts/Menlo.ttc",
];

#[cfg(not(target_os = "macos"))]
const PROBE_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
];
