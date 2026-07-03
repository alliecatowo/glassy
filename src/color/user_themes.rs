//! User-authored themes, loaded from `themes/` inside the config directory
//! (i.e. `$XDG_CONFIG_HOME/glassy/themes/` / `~/.config/glassy/themes/` on
//! Linux, `~/Library/Application Support/glassy/themes/` on macOS — the same
//! directory `glassy.conf` lives in, reusing
//! [`crate::config::parse::config_dir`]'s existing platform resolution).
//!
//! # File format
//!
//! Each `*.toml` or `*.conf` file in the directory is one theme, written as
//! flat `key = value` lines — the same shape the custom-theme editor already
//! writes into `glassy.conf` as `color.*` overrides (see
//! `app/settings_themes.rs`'s `save_custom_theme`), plus two theme-file-only
//! keys:
//!
//! ```text
//! name = My Theme             # display name; defaults to the file stem
//! light = true                # light/dark flag; defaults to false (dark)
//! color.fg = #c0caf5
//! color.bg = #1a1b26
//! color.cursor = #7dcfff       # defaults to color.fg if omitted
//! color.selection_bg = #283457 # defaults to color.bg if omitted
//! color.ansi0 = #15161e        # .. color.ansi15; unset entries default to
//!                               # Tokyo Night's (a neutral, always-defined base)
//! ```
//!
//! `color.fg` and `color.bg` are the only required keys — a file missing
//! either is malformed and is skipped (logged at `warn`), since every other
//! field can reasonably fall back from them or from a neutral default. `#`/`;`
//! prefixed lines and blank lines are ignored, matching `glassy.conf`'s own
//! parser. This is a hand-rolled parser (no `toml`/`serde` dependency), so the
//! `.toml` extension is accepted for user familiarity but not actually parsed
//! as TOML — nested tables/arrays are not supported.
//!
//! # Shadowing
//!
//! A user theme whose canonical name (its `name` key, normalized) matches a
//! built-in's replaces that built-in everywhere: [`super::theme_by_name`],
//! [`super::canonical_name`], [`super::is_light`], and [`super::theme_entries`]
//! all check user themes before consulting [`super::registry::BUILTIN_THEMES`].
//!
//! # Loading
//!
//! [`reload_user_themes`] rescans the directory and replaces the live set; it
//! is called once during startup config resolution
//! ([`crate::config::Settings::resolve`]). It never panics — a missing
//! directory is the common case (no user themes yet) and parses to an empty
//! list; a malformed individual file is logged and skipped.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use alacritty_terminal::vte::ansi::Rgb;

use super::registry::ThemeEntry;
use super::{TOKYO_NIGHT, Theme};

/// The live set of loaded user themes, in load order. `Mutex::new` is `const`,
/// so this can be a `static` without `OnceLock`/lazy-init machinery.
static USER_THEMES: Mutex<Vec<ThemeEntry>> = Mutex::new(Vec::new());

/// Rescan the user-themes directory and replace the live user-theme set.
/// Called once during startup config resolution; safe to call again (e.g. a
/// future "reload themes" command) since it fully replaces rather than
/// appends, so a file that was deleted or renamed since the last scan is
/// correctly dropped.
pub fn reload_user_themes() {
    let entries = themes_dir().map(|d| load_dir(&d)).unwrap_or_default();
    *USER_THEMES.lock().expect("USER_THEMES mutex poisoned") = entries;
}

/// Look up an already-[`normalize`](super::registry::normalize)d key against
/// the loaded user themes. Returns an owned copy (`ThemeEntry` is `Copy`) so
/// the caller never holds the lock past this call.
pub(super) fn find(key: &str) -> Option<ThemeEntry> {
    USER_THEMES
        .lock()
        .expect("USER_THEMES mutex poisoned")
        .iter()
        .find(|e| super::registry::normalize(e.canonical) == key || e.aliases.contains(&key))
        .copied()
}

/// An owned snapshot of every loaded user theme, in load order.
pub(super) fn snapshot() -> Vec<ThemeEntry> {
    USER_THEMES
        .lock()
        .expect("USER_THEMES mutex poisoned")
        .clone()
}

/// The `themes/` directory alongside `glassy.conf`, honoring the same
/// platform + `$XDG_CONFIG_HOME` resolution as the config file itself.
fn themes_dir() -> Option<PathBuf> {
    crate::config::parse::config_dir().map(|d| d.join("themes"))
}

/// Scan `dir` for `.toml`/`.conf` theme files and parse each into a
/// [`ThemeEntry`]. Files are processed in filename order, so the resulting
/// load order (and therefore the indices [`super::theme_entries`] hands out)
/// is deterministic. A directory that doesn't exist yet is the common case (no
/// user themes) and is silently treated as empty.
fn load_dir(dir: &Path) -> Vec<ThemeEntry> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = read_dir
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("toml") || e.eq_ignore_ascii_case("conf"))
                    .unwrap_or(false)
        })
        .collect();
    paths.sort();

    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for path in paths {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("theme")
            .to_string();
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse_theme_file(&text, &stem) {
                Some(entry) => {
                    // A later file whose (normalized) canonical name repeats an
                    // earlier one in the same directory wins over it, keeping
                    // `theme_entries()` free of same-directory duplicates too.
                    let key = super::registry::normalize(entry.canonical);
                    if !seen.insert(key.clone()) {
                        out.retain(|e: &ThemeEntry| super::registry::normalize(e.canonical) != key);
                    }
                    out.push(entry);
                }
                None => {
                    log::warn!(
                        "user theme '{}' is missing color.fg/color.bg; skipping",
                        path.display()
                    );
                }
            },
            Err(e) => {
                log::warn!("user theme '{}': {e}", path.display());
            }
        }
    }
    out
}

/// Parse one user-theme file's text into a [`ThemeEntry`], given the file's
/// stem (used as the default display name when the file doesn't set `name`).
///
/// Returns `None` for a malformed file — one that is missing `color.fg` or
/// `color.bg` — so the caller can log and skip it; every other field falls
/// back to a sensible default (see the module doc).
fn parse_theme_file(text: &str, stem: &str) -> Option<ThemeEntry> {
    let mut name: Option<String> = None;
    let mut light = false;
    let mut fg: Option<Rgb> = None;
    let mut bg: Option<Rgb> = None;
    let mut cursor: Option<Rgb> = None;
    let mut selection_bg: Option<Rgb> = None;
    let mut ansi16: [Option<Rgb>; 16] = [None; 16];

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = unquote(value.trim());
        if value.is_empty() {
            continue;
        }
        match key.as_str() {
            "name" => name = Some(value.to_string()),
            "light" => light = parse_bool_loose(value),
            "color.fg" => fg = hex(value, &key),
            "color.bg" => bg = hex(value, &key),
            "color.cursor" => cursor = hex(value, &key),
            "color.selection_bg" => selection_bg = hex(value, &key),
            k => {
                if let Some(idx_str) = k.strip_prefix("color.ansi")
                    && let Ok(idx) = idx_str.parse::<usize>()
                    && idx < 16
                {
                    ansi16[idx] = hex(value, &key);
                }
            }
        }
    }

    let (fg, bg) = (fg?, bg?);
    let mut final_ansi = TOKYO_NIGHT.ansi16;
    for (i, c) in ansi16.iter().enumerate() {
        if let Some(c) = c {
            final_ansi[i] = *c;
        }
    }

    let canonical_owned = name.unwrap_or_else(|| stem.to_string());
    // `ThemeEntry.canonical` is `&'static str`, matching the built-in registry
    // (whose entries are compile-time consts), so `theme_names()` and friends
    // hand out `&'static str` uniformly regardless of source. `reload_user_themes`
    // runs once at startup (and, rarely, on an explicit future reload), so
    // leaking the name here is a bounded, one-time cost proportional to the
    // number of theme files — not a per-frame or per-lookup leak like the hot
    // path this module feeds — and is reclaimed at process exit.
    let canonical: &'static str = Box::leak(canonical_owned.into_boxed_str());

    Some(ThemeEntry {
        canonical,
        aliases: &[],
        light,
        theme: Theme {
            fg,
            bg,
            cursor: cursor.unwrap_or(fg),
            selection_bg: selection_bg.unwrap_or(bg),
            ansi16: final_ansi,
        },
    })
}

/// Parse a hex color value for `key`, logging + returning `None` on failure
/// rather than aborting the whole file (a single bad entry just falls back to
/// its default, same as an omitted key).
fn hex(value: &str, key: &str) -> Option<Rgb> {
    match crate::config::parse::parse_hex_color(value) {
        Ok(rgb) => Some(rgb),
        Err(e) => {
            log::warn!("user theme: invalid {key} = '{value}': {e:#}");
            None
        }
    }
}

/// Loose boolean parse for the optional `light` key: any of the common
/// truthy spellings; anything else (including absence) is `false`. Unlike
/// `glassy.conf`'s `parse_bool`, a malformed value here doesn't fail the whole
/// file — `light` is cosmetic (it only affects follow-system defaulting).
fn parse_bool_loose(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "on" | "1"
    )
}

/// Strip one layer of matching single or double quotes from `s`, if present.
fn unquote(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (a, b) = (bytes[0], bytes[bytes.len() - 1]);
        if (a == b'"' && b == b'"') || (a == b'\'' && b == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    #[test]
    fn parses_a_valid_theme_file() {
        let text = "\
name = My Theme
light = true
color.fg = #c0caf5
color.bg = #1a1b26
color.cursor = #7dcfff
color.selection_bg = #283457
color.ansi0 = #15161e
color.ansi1 = #f7768e
";
        let entry = parse_theme_file(text, "my-theme").expect("parses");
        assert_eq!(entry.canonical, "My Theme");
        assert!(entry.light);
        assert_eq!(entry.theme.fg, rgb(0xc0, 0xca, 0xf5));
        assert_eq!(entry.theme.bg, rgb(0x1a, 0x1b, 0x26));
        assert_eq!(entry.theme.cursor, rgb(0x7d, 0xcf, 0xff));
        assert_eq!(entry.theme.selection_bg, rgb(0x28, 0x34, 0x57));
        assert_eq!(entry.theme.ansi16[0], rgb(0x15, 0x16, 0x1e));
        assert_eq!(entry.theme.ansi16[1], rgb(0xf7, 0x76, 0x8e));
        assert!(entry.aliases.is_empty());
    }

    #[test]
    fn missing_fg_or_bg_skips_the_file() {
        assert!(parse_theme_file("color.fg = #ffffff\n", "incomplete").is_none());
        assert!(parse_theme_file("color.bg = #000000\n", "incomplete").is_none());
        assert!(parse_theme_file("light = true\n", "incomplete").is_none());
        assert!(parse_theme_file("color.fg = #ffffff\ncolor.bg = #000000\n", "minimal").is_some());
    }

    #[test]
    fn light_flag_defaults_to_false_and_parses_common_spellings() {
        let base = "color.fg = #ffffff\ncolor.bg = #000000\n";
        assert!(!parse_theme_file(base, "t").unwrap().light, "no light key");
        for v in ["true", "TRUE", "1", "yes", "on"] {
            let text = format!("{base}light = {v}\n");
            assert!(parse_theme_file(&text, "t").unwrap().light, "light={v}");
        }
        for v in ["false", "0", "no", "off", "banana"] {
            let text = format!("{base}light = {v}\n");
            assert!(!parse_theme_file(&text, "t").unwrap().light, "light={v}");
        }
    }

    #[test]
    fn name_defaults_to_file_stem() {
        let text = "color.fg = #ffffff\ncolor.bg = #000000\n";
        let entry = parse_theme_file(text, "sunset-glow").expect("parses");
        assert_eq!(entry.canonical, "sunset-glow");
    }

    #[test]
    fn missing_cursor_and_selection_default_from_fg_bg() {
        let text = "color.fg = #ffffff\ncolor.bg = #000000\n";
        let entry = parse_theme_file(text, "t").expect("parses");
        assert_eq!(entry.theme.cursor, entry.theme.fg);
        assert_eq!(entry.theme.selection_bg, entry.theme.bg);
    }

    #[test]
    fn unspecified_ansi_entries_fall_back_to_tokyo_night() {
        let text = "color.fg = #ffffff\ncolor.bg = #000000\ncolor.ansi1 = #ff0000\n";
        let entry = parse_theme_file(text, "t").expect("parses");
        assert_eq!(entry.theme.ansi16[1], rgb(0xff, 0x00, 0x00));
        assert_eq!(entry.theme.ansi16[2], TOKYO_NIGHT.ansi16[2]);
    }

    #[test]
    fn malformed_hex_falls_back_instead_of_failing_the_file() {
        let text = "color.fg = #ffffff\ncolor.bg = #000000\ncolor.cursor = not-a-color\n";
        let entry = parse_theme_file(text, "t").expect("still parses");
        // cursor falls back to fg since the malformed value is dropped.
        assert_eq!(entry.theme.cursor, entry.theme.fg);
    }

    #[test]
    fn quoted_values_are_unquoted() {
        let text = "name = \"Quoted Name\"\ncolor.fg = '#ffffff'\ncolor.bg = \"#000000\"\n";
        let entry = parse_theme_file(text, "t").expect("parses");
        assert_eq!(entry.canonical, "Quoted Name");
        assert_eq!(entry.theme.fg, rgb(0xff, 0xff, 0xff));
        assert_eq!(entry.theme.bg, rgb(0x00, 0x00, 0x00));
    }

    #[test]
    fn load_dir_scans_toml_and_conf_skips_other_extensions_and_sorts() {
        let dir = unique_temp_dir("scan");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(
            dir.join("b-theme.toml"),
            "color.fg=#ffffff\ncolor.bg=#000000\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("a-theme.conf"),
            "color.fg=#ffffff\ncolor.bg=#000000\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ignored.txt"),
            "color.fg=#ffffff\ncolor.bg=#000000\n",
        )
        .unwrap();
        std::fs::write(dir.join("broken.toml"), "light = true\n").unwrap(); // no fg/bg

        let entries = load_dir(&dir);
        let names: Vec<&str> = entries.iter().map(|e| e.canonical).collect();
        assert_eq!(names, vec!["a-theme", "b-theme"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_dir_on_missing_directory_returns_empty() {
        let dir = unique_temp_dir("missing");
        assert!(load_dir(&dir).is_empty());
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "glassy-user-themes-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
