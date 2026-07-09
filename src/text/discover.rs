//! Font discovery: fc-match cache, candidate producers, and fallback font
//! loading. All Linux-specific subprocess/fontconfig work lives here so the
//! shaping layer (`shape.rs`) stays free of platform-specific I/O.

#[cfg(target_os = "linux")]
use std::collections::HashSet;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Command;
use std::sync::Arc;

use cosmic_text::fontdb;

/// Path to the on-disk fc-match resolution cache.
///
/// Layout: one entry per line, tab-separated `pattern\tfile_path`. The cache
/// prevents repeat `fc-match` subprocess invocations on subsequent glassy
/// launches (the "fc-match storm" at startup). Invalid / stale entries are
/// ignored — a failed lookup just falls through to a live `fc-match` call and
/// refreshes the cache entry.
#[cfg(target_os = "linux")]
fn fc_cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("glassy/fc-cache.tsv"))
}

/// Load the entire fc-match cache into a `HashMap<pattern, file_path>`.
/// Silently returns an empty map on any I/O or parse error.
#[cfg(target_os = "linux")]
pub(super) fn fc_cache_load() -> std::collections::HashMap<String, String> {
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
            && !k.is_empty()
            && !v.is_empty()
        {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

/// Persist a single `pattern → file_path` mapping into the fc-match cache.
/// Creates the parent directory if absent. Errors are logged at debug level
/// and do not abort the font load.
#[cfg(target_os = "linux")]
pub(super) fn fc_cache_insert(pattern: &str, file_path: &str) {
    let path = match fc_cache_path() {
        Some(p) => p,
        None => return,
    };
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        log::debug!("glassy: fc-cache dir create failed: {e}");
        return;
    }
    // Append-only: one line per entry. The cache grows monotonically; a stale
    // entry is harmless because lookup also validates the path still exists.
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{pattern}\t{file_path}") {
                log::debug!("glassy: fc-cache write failed: {e}");
            }
        }
        Err(e) => log::debug!("glassy: fc-cache open failed: {e}"),
    }
}

/// The owned font family selection for shaping.
pub(super) enum FamilyOwned {
    Monospace,
    Named(String),
}

impl FamilyOwned {
    pub(super) fn as_family(&self) -> cosmic_text::Family<'_> {
        match self {
            FamilyOwned::Monospace => cosmic_text::Family::Monospace,
            FamilyOwned::Named(name) => cosmic_text::Family::Name(name),
        }
    }
}

/// A font candidate produced by discovery: the raw file bytes, the resolved file
/// path it came from (when it originated from a concrete file, used to de-dup the
/// primary against the fallback chain), and a short label describing its origin
/// (used only for logging/diagnostics).
pub(super) struct FontCandidate {
    pub(super) bytes: Vec<u8>,
    /// Absolute file path the bytes were read from, if known. `None` only when a
    /// candidate's bytes did not come from a single on-disk file.
    pub(super) path: Option<PathBuf>,
    pub(super) source_label: String,
}

/// The outcome of building a `FontSystem` from a single candidate's bytes: the
/// constructed system, the owned family to shape with, and whether the face we
/// loaded actually reports itself as monospaced.
pub(super) struct LoadedFont {
    pub(super) font_system: cosmic_text::FontSystem,
    pub(super) family: FamilyOwned,
    pub(super) is_monospaced: bool,
    /// Family name of the emoji fallback font in the database, if one was loaded.
    /// Used to force ZWJ clusters (compound emoji like 🏳️‍⚧️) into a single font
    /// run so the GSUB ZWJ ligature can be resolved — shaping across font boundaries
    /// silently drops the ZWJ join.
    pub(super) emoji_family: Option<String>,
    /// Resolved per-style family overrides (may be all `None` if no overrides
    /// were configured). The shaper uses these to pick the right family name
    /// when rendering bold / italic / bold-italic cells.
    pub(super) style_families: StyleFamilies,
}

/// Per-style font family overrides from config (`font_bold`, `font_italic`,
/// `font_bold_italic`). Each field is an optional family name or absolute file
/// path. When present, the named face is loaded into the fontdb in addition to
/// the primary font's synthesized style faces. `None` in any field falls back to
/// the synthesized style (or the fontconfig-resolved style face, if available).
#[derive(Default)]
pub(super) struct StyleOverrides {
    pub bold: Option<String>,
    pub italic: Option<String>,
    pub bold_italic: Option<String>,
}

/// A resolved per-style family, stored in `LoadedFont` so the shaper can pick
/// the right family name for each (bold, italic, bold-italic) combination.
pub(super) struct StyleFamilies {
    /// Family name for bold text. `None` → use the primary family at `Weight::BOLD`.
    pub bold: Option<String>,
    /// Family name for italic text. `None` → use the primary family at `Style::Italic`.
    pub italic: Option<String>,
    /// Family name for bold-italic text. `None` → primary + `Weight::BOLD + Style::Italic`.
    pub bold_italic: Option<String>,
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
/// `style_overrides` provides optional explicit families for bold / italic /
/// bold-italic; when set the named family (or file) is loaded into the same
/// fontdb so cosmic-text resolves styled text with the override instead of
/// synthesising it from the primary.
///
/// We deliberately avoid `FontSystem::new()` (a full system scan) here — only a
/// handful of fontconfig-resolved fallback files are loaded.
///
/// Returns `None` if the primary bytes contained no usable face.
pub(super) fn build_font_system(
    bytes: Vec<u8>,
    primary_path: Option<PathBuf>,
    style_overrides: &StyleOverrides,
) -> Option<LoadedFont> {
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
    #[cfg(target_os = "linux")]
    let fc_cache = fc_cache_load();

    // Load the bold/italic faces of the same family so styled text shapes with
    // the real monospace face rather than falling back to a proportional font.
    #[cfg(target_os = "linux")]
    load_primary_styles(&mut db, &family_name, primary_path.as_deref(), &fc_cache);

    // Load per-style override fonts (font_bold / font_italic / font_bold_italic).
    // Each override is an explicit family name or absolute file path. We resolve
    // it into the fontdb so the shaper can address it by family name.
    #[cfg(target_os = "linux")]
    let style_families =
        load_style_override_fonts(&mut db, style_overrides, primary_path.as_deref(), &fc_cache);
    #[cfg(target_os = "macos")]
    let style_families = {
        let cache = macos_font_cache_load();
        load_style_override_fonts_macos(&mut db, style_overrides, primary_path.as_deref(), &cache)
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let style_families = {
        let _ = style_overrides;
        StyleFamilies {
            bold: None,
            italic: None,
            bold_italic: None,
        }
    };

    // Enrich the database with fallback faces (best-effort; failures are skipped).
    #[cfg(target_os = "linux")]
    load_fallback_fonts(&mut db, primary_path.as_deref(), &fc_cache);
    #[cfg(target_os = "macos")]
    load_fallback_fonts_macos(&mut db, primary_path.as_deref());
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = primary_path;

    // With the fallback chain loaded, record the emoji face's family name (the
    // first face whose family contains "emoji") so the shaper can force ZWJ
    // clusters into a single run. See `LoadedFont::emoji_family`.
    let emoji_family = db
        .faces()
        .find(|f| {
            f.families
                .iter()
                .any(|(name, _)| name.to_lowercase().contains("emoji"))
        })
        .and_then(|f| f.families.first().map(|(n, _)| n.clone()));
    if let Some(ref ef) = emoji_family {
        log::debug!("glassy: emoji family for ZWJ shaping: '{ef}'");
    }

    let font_system = cosmic_text::FontSystem::new_with_locale_and_db("en-US".to_string(), db);
    Some(LoadedFont {
        font_system,
        family: FamilyOwned::Named(family_name),
        is_monospaced,
        emoji_family,
        style_families,
    })
}

/// Load the per-style override fonts (`font_bold`, `font_italic`, `font_bold_italic`)
/// into `db` on Linux. Each override is either an absolute file path or a family
/// name resolved via fontconfig. Returns the resolved family names so the shaper
/// can address each style.
#[cfg(target_os = "linux")]
fn load_style_override_fonts(
    db: &mut fontdb::Database,
    overrides: &StyleOverrides,
    primary_path: Option<&Path>,
    cache: &std::collections::HashMap<String, String>,
) -> StyleFamilies {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    if let Some(p) = primary_path {
        seen.insert(canonical_or_owned(p));
    }
    let mut resolve = |name: &str| -> Option<String> {
        // Allow explicit file paths.
        let as_path = Path::new(name);
        if as_path.is_file() {
            let key = canonical_or_owned(as_path);
            if seen.insert(key) {
                load_font_file(db, as_path);
            }
            // Read the family name from the newly-loaded face.
            return db
                .faces()
                .last()
                .and_then(|f| f.families.first().map(|(n, _)| n.clone()));
        }
        // Otherwise treat as a family name and resolve via fontconfig.
        if let Some(path) = fc_match_family_cached(name, cache) {
            if !cache.contains_key(&format!("family:{name}")) {
                fc_cache_insert(&format!("family:{name}"), &path);
            }
            let key = canonical_or_owned(Path::new(&path));
            if seen.insert(key) && load_font_file(db, Path::new(&path)) {
                log::debug!("glassy: loaded style override '{name}': {path}");
            }
            // The family name from fc-match could differ from `name`; re-query the db.
            return db
                .faces()
                .last()
                .and_then(|f| f.families.first().map(|(n, _)| n.clone()));
        }
        log::warn!("glassy: font style override '{name}' not found; using primary style");
        None
    };
    StyleFamilies {
        bold: overrides.bold.as_deref().and_then(&mut resolve),
        italic: overrides.italic.as_deref().and_then(&mut resolve),
        bold_italic: overrides.bold_italic.as_deref().and_then(&mut resolve),
    }
}

/// macOS variant: resolve style override fonts from the macOS font cache.
#[cfg(target_os = "macos")]
fn load_style_override_fonts_macos(
    db: &mut fontdb::Database,
    overrides: &StyleOverrides,
    primary_path: Option<&Path>,
    cache: &std::collections::HashMap<String, String>,
) -> StyleFamilies {
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    if let Some(p) = primary_path {
        seen.insert(p.to_path_buf());
    }
    let mut resolve = |name: &str| -> Option<String> {
        let path = find_macos_font_file(name, cache)?;
        if seen.insert(path.clone()) {
            load_font_file_macos(db, &path);
        }
        db.faces()
            .last()
            .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
    };
    StyleFamilies {
        bold: overrides.bold.as_deref().and_then(&mut resolve),
        italic: overrides.italic.as_deref().and_then(&mut resolve),
        bold_italic: overrides.bold_italic.as_deref().and_then(resolve),
    }
}

/// Fallback families to resolve via fontconfig and add to the database, in order.
/// Each entry is an `fc-match` pattern; we resolve it to a concrete file with
/// `fc-match -f %{file} "<pattern>"`. Multiple patterns cover the same script so
/// that whichever the host actually has installed gets pulled in.
// Emoji is handled separately (see `load_emoji_fallback`): we load a bundled
// CBDT color-bitmap face by path, because swash cannot rasterize the COLRv1
// "Noto Color Emoji" that fontconfig resolves to on most hosts.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
            Spawned(
                std::thread::ScopedJoinHandle<'scope, Option<String>>,
                std::marker::PhantomData<&'env ()>,
            ),
        }
        let work: Vec<(&str, Resolution<'_, '_>)> = FALLBACK_PATTERNS
            .iter()
            .map(|pattern| {
                if let Some(cached_path) = cache.get(*pattern)
                    && Path::new(cached_path).exists()
                {
                    return (*pattern, Resolution::Cached(Some(cached_path.clone())));
                }
                let handle = s.spawn(move || fc_match_file_live(pattern));
                (
                    *pattern,
                    Resolution::Spawned(handle, std::marker::PhantomData),
                )
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
#[cfg(target_os = "linux")]
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
                    && Path::new(cached_path).exists()
                {
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn fc_match_file_cached(
    pattern: &str,
    cache: &std::collections::HashMap<String, String>,
) -> Option<String> {
    // Check the disk cache first — a valid hit avoids the subprocess.
    if let Some(cached_path) = cache.get(pattern)
        && Path::new(cached_path).exists()
    {
        log::debug!("glassy: fc-cache hit for '{pattern}': {cached_path}");
        return Some(cached_path.clone());
    }
    fc_match_file_live(pattern)
}

/// Run a live `fc-match` subprocess (no cache involved).
#[cfg(target_os = "linux")]
pub(super) fn fc_match_file_live(pattern: &str) -> Option<String> {
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
pub(super) type CandidateProducer = Box<dyn FnOnce() -> Option<FontCandidate>>;

/// Build the ordered chain of lazy candidate *producers*. Returning closures
/// (rather than eagerly materializing every candidate) lets [`Text::load`] stop
/// at the first producer that yields a usable monospace face, so a host with a
/// good default font never pays the `fc-match` + read cost of the rest of the
/// chain. Order: explicit override, requested family, curated families, generic
/// monospace, then known install paths.
pub(super) fn discover_font_producers(requested: Option<&str>) -> Vec<CandidateProducer> {
    let mut producers: Vec<CandidateProducer> = Vec::new();

    // Load the fc-match resolution cache once upfront. Curated-family closures
    // capture a clone; a cache hit in the closure avoids the subprocess entirely.
    #[cfg(target_os = "linux")]
    let fc_cache = fc_cache_load();
    // macOS equivalent: family → file_path cache that survives across launches,
    // avoiding directory scans on warm starts.
    #[cfg(target_os = "macos")]
    let macos_cache = macos_font_cache_load();

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
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "macos")]
    if let Some(name) = requested {
        let name = name.trim().to_string();
        if !name.is_empty() {
            let cache_clone = macos_cache.clone();
            producers.push(Box::new(move || {
                let path = find_macos_font_file(&name, &cache_clone)?;
                let bytes = read_font(&path)?;
                Some(FontCandidate {
                    bytes,
                    path: Some(path),
                    source_label: format!("font_family {name} (macos)"),
                })
            }));
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = requested;

    // 2. A curated list of good monospace families, each resolved to a concrete
    //    file via fontconfig and verified to actually *be* that family (fc-match
    //    returns a nearest fallback even when the family is absent). One producer
    //    per family so discovery stops at the first installed one.
    #[cfg(target_os = "linux")]
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

    // 2b. macOS: prefer any installed Nerd Font monospace, discovered by property
    //     (family name contains "nerd font" + monospaced) rather than a hardcoded
    //     family list. This gives icon/powerline coverage in the primary font when
    //     the user has any Nerd Font installed, without naming specific fonts.
    #[cfg(target_os = "macos")]
    producers.push(Box::new(move || {
        let path = find_macos_nerd_font()?;
        let bytes = read_font(&path)?;
        Some(FontCandidate {
            bytes,
            path: Some(path.clone()),
            source_label: format!("nerd font ({})", path.display()),
        })
    }));

    // 2c. macOS: the OS default monospace, resolved live via CoreText
    //     (kCTFontUserFixedPitchFontType). No hardcoded paths — this is whatever
    //     the system designates (SF Mono / Menlo). Any glyph the primary font
    //     lacks (icons, emoji, CJK) is resolved per-glyph by the CoreText cascade
    //     in shape.rs, so this primary need not cover everything.
    #[cfg(target_os = "macos")]
    producers.push(Box::new(|| {
        let path = coretext_monospace_path()?;
        let bytes = read_font(&path)?;
        Some(FontCandidate {
            bytes,
            path: Some(path.clone()),
            source_label: format!("coretext system monospace ({})", path.display()),
        })
    }));

    // 3. Generic monospace via fontconfig; always a real monospace face.
    #[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
pub(super) fn read_font(path: impl AsRef<Path>) -> Option<Vec<u8>> {
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn fc_match_family_cached(
    family: &str,
    cache: &std::collections::HashMap<String, String>,
) -> Option<String> {
    // For family lookups we store the key as "family:<name>" to avoid
    // collisions with bare `fc_match_file` pattern keys.
    let key = format!("family:{family}");
    if let Some(cached_path) = cache.get(&key)
        && Path::new(cached_path).exists()
    {
        log::debug!("glassy: fc-cache hit for family '{family}': {cached_path}");
        return Some(cached_path.clone());
    }
    fc_match_family_live(family)
}

/// Run a live `fc-match` family lookup (no cache involved).
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn fc_match_monospace_cached(cache: &std::collections::HashMap<String, String>) -> Option<String> {
    let key = "monospace";
    if let Some(cached_path) = cache.get(key)
        && Path::new(cached_path).exists()
    {
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

/// macOS resolves its monospace primary live via CoreText (see
/// `coretext_monospace_path`), so there are no hardcoded probe paths here.
#[cfg(target_os = "macos")]
const PROBE_PATHS: &[&str] = &[];

#[cfg(not(target_os = "macos"))]
const PROBE_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
];

/// Path to the macOS font-discovery cache (mirrors the Linux fc-cache).
///
/// Layout: one entry per line, tab-separated `family\tfile_path`. A cache hit
/// skips the directory scan entirely on warm launches. Stale entries (missing
/// file) are silently ignored and a live scan fills the gap.
#[cfg(target_os = "macos")]
fn macos_font_cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches")))?;
    Some(base.join("glassy/macos-font-cache.tsv"))
}

/// Load the macOS font cache into a `HashMap<family, file_path>`.
#[cfg(target_os = "macos")]
pub(super) fn macos_font_cache_load() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let path = match macos_font_cache_path() {
        Some(p) => p,
        None => return map,
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return map,
    };
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('\t')
            && !k.is_empty()
            && !v.is_empty()
        {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

/// Persist a `family → file_path` mapping to the macOS font cache.
#[cfg(target_os = "macos")]
fn macos_font_cache_insert(family: &str, file_path: &str) {
    let path = match macos_font_cache_path() {
        Some(p) => p,
        None => return,
    };
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        log::debug!("glassy: macos-font-cache dir create failed: {e}");
        return;
    }
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{family}\t{file_path}") {
                log::debug!("glassy: macos-font-cache write failed: {e}");
            }
        }
        Err(e) => log::debug!("glassy: macos-font-cache open failed: {e}"),
    }
}

/// Font directories to search on macOS, in priority order.
#[cfg(target_os = "macos")]
fn macos_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Library/Fonts"));
    }
    dirs.push(PathBuf::from("/Library/Fonts"));
    dirs.push(PathBuf::from("/System/Library/Fonts"));
    dirs
}

/// Curated, well-known Nerd Font families to prefer when more than one is
/// installed, in priority order — the macOS analogue of Linux's
/// `CURATED_FAMILIES` (which fontconfig can query by name; macOS discovery
/// here works from raw filenames instead, since there's no macOS equivalent
/// of `fc-match` used for this pass). Matched as a substring against the
/// normalized file stem (spaces stripped, lowercased — the same normalization
/// `find_macos_nerd_font` already applies), so e.g.
/// "JetBrainsMonoNerdFontMono-Regular.ttf" matches "jetbrainsmono". A font not
/// on this list still gets picked (Nerd Font coverage matters more than which
/// specific family) — it just sorts after every curated family, in a stable
/// alphabetical order rather than directory-scan order.
#[cfg(target_os = "macos")]
const CURATED_NERD_FONT_FAMILIES: &[&str] = &[
    "jetbrainsmono",
    "firacode",
    "cascadiacode",
    "hack",
    "meslo",
    "sourcecodepro",
    "iosevka",
    "ubuntumono",
    "dejavusansmono",
];

/// Where a candidate's normalized stem falls in `CURATED_NERD_FONT_FAMILIES`
/// (lower = more preferred); anything not on the list ranks after all of them.
#[cfg(target_os = "macos")]
fn curated_nerd_font_rank(normalized_stem: &str) -> usize {
    CURATED_NERD_FONT_FAMILIES
        .iter()
        .position(|family| normalized_stem.contains(family))
        .unwrap_or(CURATED_NERD_FONT_FAMILIES.len())
}

/// Find any installed Nerd Font monospace by *property* — the normalized file
/// stem contains "nerdfont" — rather than matching one specific family name.
/// The "Mono" variant (single-cell icons) and the Regular weight are
/// preferred so a terminal gets even-width icons.
///
/// Among files that tie on the mono/regular preference, the pick used to be
/// whichever `std::fs::read_dir` happened to enumerate first — filesystem
/// directory-entry order, which is NOT alphabetical or otherwise meaningful
/// (APFS returns entries in an internal order that can shift across font
/// (re)installs). A user with several Nerd Font families installed (JetBrains
/// Mono, other coding fonts, accessibility fonts, …) had no guarantee which
/// one glassy would load, and no way to find out short of reading debug logs.
///
/// Now every candidate in a pass is collected across all scanned directories
/// FIRST, then ranked by (curated-family preference, normalized stem) before
/// picking the best one — deterministic across runs and reinstalls, and it
/// prefers a well-known complete family when one is present. The resolved
/// family name is surfaced to the user via `Text::resolved_family_name` / the
/// Settings panel (General, next to the Font dropdown), so this no longer
/// needs to be discovered by reading logs either.
#[cfg(target_os = "macos")]
fn find_macos_nerd_font() -> Option<PathBuf> {
    let dirs = macos_font_dirs();
    // Two passes: first require the "mono" + "regular" variant, then accept any
    // Nerd Font face. Each pass scans user dirs before system dirs for
    // collection purposes, but ALL matches across ALL dirs in the pass are
    // ranked together, so a curated family in a later dir still wins over a
    // less-preferred one in an earlier dir.
    for (want_mono, want_regular) in [(true, true), (true, false), (false, false)] {
        let mut candidates: Vec<(PathBuf, String)> = Vec::new();
        for dir in &dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let fname_str = fname.to_string_lossy();
                let ext = fname_str.rsplit('.').next().map(|e| e.to_ascii_lowercase());
                if !matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
                    continue;
                }
                let stem = fname_str
                    .rsplit_once('.')
                    .map(|(s, _)| s)
                    .unwrap_or(&fname_str);
                let n = stem.replace(' ', "").to_lowercase();
                if !n.contains("nerdfont") {
                    continue;
                }
                if want_mono && !n.contains("nerdfontmono") {
                    continue;
                }
                if want_regular && !n.contains("regular") {
                    continue;
                }
                // Skip explicit non-regular weights/styles on the regular passes.
                if want_regular
                    && (n.contains("bold")
                        || n.contains("italic")
                        || n.contains("light")
                        || n.contains("thin"))
                {
                    continue;
                }
                candidates.push((entry.path(), n));
            }
        }
        if candidates.is_empty() {
            continue;
        }
        candidates.sort_by(|(_, a), (_, b)| {
            curated_nerd_font_rank(a)
                .cmp(&curated_nerd_font_rank(b))
                .then_with(|| a.cmp(b))
        });
        let (path, _) = &candidates[0];
        log::debug!("glassy: macos nerd font: {}", path.display());
        return Some(path.clone());
    }
    None
}

/// Resolve the OS default monospace font's file path via CoreText
/// (`kCTFontUserFixedPitchFontType` — whatever the system designates, e.g. SF
/// Mono or Menlo). No hardcoded paths: CoreText reports the live system choice.
#[cfg(target_os = "macos")]
fn coretext_monospace_path() -> Option<PathBuf> {
    use core_foundation::url::CFURL;
    use core_text::font::{kCTFontUserFixedPitchFontType, new_ui_font_for_language};

    let font = new_ui_font_for_language(kCTFontUserFixedPitchFontType, 14.0, None);
    let url: CFURL = font.url()?;
    // CFURL → filesystem path. `to_path()` yields a PathBuf for file:// URLs.
    url.to_path()
}

/// Search macOS font directories for a font file matching `family`, checking
/// `cache` first so warm launches skip the directory scan entirely.
///
/// If `family` is an absolute path to an existing file it is returned as-is.
/// Otherwise a cache hit (valid path still on disk) returns immediately. On a
/// miss, directories are scanned: first pass prefers Regular-weight stems,
/// second accepts any weight. A successful scan result is written to the cache.
#[cfg(target_os = "macos")]
pub(super) fn find_macos_font_file(
    family: &str,
    cache: &std::collections::HashMap<String, String>,
) -> Option<PathBuf> {
    let as_path = Path::new(family);
    if as_path.is_file() {
        return Some(as_path.to_path_buf());
    }
    // Cache hit: skip the directory scan.
    if let Some(cached) = cache.get(family)
        && Path::new(cached).exists()
    {
        log::debug!("glassy: macos-font-cache hit for '{family}': {cached}");
        return Some(PathBuf::from(cached));
    }
    let needle = family.replace(' ', "").to_lowercase();
    let dirs = macos_font_dirs();
    for prefer_regular in [true, false] {
        for dir in &dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let fname_str = fname.to_string_lossy();
                let ext = fname_str.rsplit('.').next().map(|e| e.to_ascii_lowercase());
                if !matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
                    continue;
                }
                let stem = fname_str
                    .rsplit_once('.')
                    .map(|(s, _)| s)
                    .unwrap_or(&fname_str);
                let normalized = stem.replace(' ', "").to_lowercase();
                if !normalized.starts_with(&needle) {
                    continue;
                }
                if prefer_regular {
                    let has_weight_suffix = normalized.contains("bold")
                        || normalized.contains("italic")
                        || normalized.contains("light")
                        || normalized.contains("medium")
                        || normalized.contains("extrabold")
                        || normalized.contains("extralight")
                        || normalized.contains("semibold")
                        || normalized.contains("thin");
                    if has_weight_suffix && !normalized.contains("regular") {
                        continue;
                    }
                }
                let path = entry.path();
                log::debug!(
                    "glassy: macos found font for '{family}': {}",
                    path.display()
                );
                // Persist to cache for next launch.
                if !cache.contains_key(family) {
                    macos_font_cache_insert(family, &path.to_string_lossy());
                }
                return Some(path);
            }
        }
    }
    None
}

/// Load fallback fonts into `db` on macOS: Apple Color Emoji and Apple Symbols.
/// Simple emoji (single codepoint + variation selectors) render correctly via
/// Apple Color Emoji's SBIX bitmaps. ZWJ compound sequences (🏳️‍⚧️ etc.) can't be
/// shaped from this database (rustybuzz drops Apple's GSUB ZWJ chain); the shaping
/// layer renders those directly via CoreText (see `shape::rasterize_cluster`).
#[cfg(target_os = "macos")]
fn load_fallback_fonts_macos(db: &mut fontdb::Database, primary_path: Option<&Path>) {
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    if let Some(p) = primary_path {
        seen.insert(p.to_path_buf());
    }
    for path in [
        "/System/Library/Fonts/Apple Color Emoji.ttc",
        "/System/Library/Fonts/Apple Symbols.ttf",
        "/System/Library/Fonts/Symbol.ttf",
    ] {
        let p = Path::new(path);
        if seen.insert(p.to_path_buf()) {
            load_font_file_macos(db, p);
        }
    }
}

/// Load a single font file into `db` on macOS.
#[cfg(target_os = "macos")]
fn load_font_file_macos(db: &mut fontdb::Database, path: &Path) -> bool {
    match db.load_font_file(path) {
        Ok(()) => {
            log::debug!("glassy: loaded fallback font: {}", path.display());
            true
        }
        Err(err) => {
            log::debug!("glassy: skipping font {}: {err}", path.display());
            false
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_nerd_font_tests {
    use super::*;

    /// Curated families must rank strictly ahead of anything not on the list,
    /// and earlier entries must rank ahead of later ones.
    #[test]
    fn curated_rank_orders_known_families_first() {
        assert!(
            curated_nerd_font_rank("jetbrainsmononerdfontmono-regular")
                < curated_nerd_font_rank("firacodenerdfontmono-regular")
        );
        assert!(
            curated_nerd_font_rank("firacodenerdfontmono-regular")
                < curated_nerd_font_rank("somerandomfontnerdfontmono-regular")
        );
    }

    /// A stem matching no curated family ranks exactly at the "unranked"
    /// sentinel (list length), not some arbitrary lower/higher value.
    #[test]
    fn curated_rank_unranked_sentinel() {
        assert_eq!(
            curated_nerd_font_rank("intonemononerdfontmono-regular"),
            CURATED_NERD_FONT_FAMILIES.len()
        );
    }

    /// Real end-to-end check against whatever this machine actually has
    /// installed: calling `find_macos_nerd_font` twice must return the exact
    /// same path — the whole point of ranking every candidate instead of
    /// taking the first directory-scan hit. Skips gracefully if no Nerd Font
    /// is installed (e.g. a bare CI runner).
    #[test]
    fn find_macos_nerd_font_is_deterministic() {
        let Some(first) = find_macos_nerd_font() else {
            eprintln!("find_macos_nerd_font_is_deterministic: skipped (no Nerd Font installed)");
            return;
        };
        for _ in 0..5 {
            assert_eq!(
                find_macos_nerd_font(),
                Some(first.clone()),
                "repeated calls must resolve to the identical font file"
            );
        }
    }
}
