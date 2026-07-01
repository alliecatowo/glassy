//! Configuration file parsing: RawConfig accumulation, file I/O, and value parsing.

use alacritty_terminal::tty::Shell;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::app::Config;
use crate::color;
use crate::renderer::DEFAULT_OPACITY;

use super::keymap::{build_keymap, default_keymap};
use super::platform::Platform;

const DEFAULT_FONT_SIZE: f32 = 14.0;
const DEFAULT_SCROLLBACK: usize = 10_000;
/// Default number of recently-run commands retained for the command palette's
/// history source (OSC 133 `B`..`C` capture). 0 disables it.
const DEFAULT_COMMAND_HISTORY: usize = 200;
/// Default minimum command duration (ms) that triggers a command-finish desktop
/// notification when the window is unfocused. 10 s avoids spamming for quick
/// commands while still catching long builds/tests.
const DEFAULT_NOTIFY_COMMAND_THRESHOLD_MS: u64 = 10_000;

/// A single entry in the `font_symbol_map` config key: a Unicode range mapped
/// to a specific font family. The shaper routes codepoints in `[start, end]`
/// (inclusive) to the named family instead of the primary font.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolMapEntry {
    /// First codepoint of the range (inclusive).
    pub start: u32,
    /// Last codepoint of the range (inclusive; equal to `start` for a single
    /// codepoint mapping).
    pub end: u32,
    /// Font family name (or absolute file path) to route this range to.
    pub family: String,
}

/// Parse the `font_symbol_map` value into a `Vec<SymbolMapEntry>`.
///
/// Each entry is `"RANGE:Family"` where RANGE is `U+XXXX` or `U+XXXX-U+YYYY`
/// (hex codepoints). Multiple entries are separated by commas. Entries that
/// cannot be parsed are logged at warn level and skipped.
///
/// Examples:
/// ```text
/// U+E000-U+F8FF : Symbols Nerd Font Mono
/// U+2500-U+257F : FiraCode Nerd Font Mono, U+1F600-U+1F64F : Noto Color Emoji
/// ```
pub fn parse_symbol_map(value: &str) -> Vec<SymbolMapEntry> {
    let mut out = Vec::new();
    // Split on commas first (entries), then parse each.
    for entry in value.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        // The colon separator between range and family. Use rfind so a family
        // name containing ':' (unlikely but safe) doesn't confuse the split.
        // Actually we need the FIRST colon that appears after a codepoint range.
        // Ranges look like "U+XXXX" or "U+XXXX-U+YYYY" — find the colon after
        // the range token.
        //
        // Strategy: split on ':' and reconstruct family from tail parts.
        let parts: Vec<&str> = entry.splitn(2, ':').collect();
        if parts.len() != 2 {
            log::warn!("glassy: font_symbol_map: expected 'RANGE:Family', got '{entry}'; skipping");
            continue;
        }
        let range_str = parts[0].trim();
        let family = parts[1].trim().to_string();
        if family.is_empty() {
            log::warn!("glassy: font_symbol_map: empty family in '{entry}'; skipping");
            continue;
        }
        // Parse "U+XXXX" or "U+XXXX-U+YYYY".
        let (start, end) = if let Some((lo, hi)) = range_str.split_once('-') {
            // "U+XXXX-U+YYYY" — note the split_once('-') will split at the
            // first dash, which could be the dash between U+XXXX and U+YYYY.
            // But "U+E000-U+F8FF" splits as lo="U+E000" hi="U+F8FF". Correct.
            let s = parse_codepoint(lo.trim());
            let e = parse_codepoint(hi.trim());
            match (s, e) {
                (Some(s), Some(e)) => (s, e.max(s)),
                _ => {
                    log::warn!("glassy: font_symbol_map: invalid range '{range_str}'; skipping");
                    continue;
                }
            }
        } else {
            match parse_codepoint(range_str) {
                Some(cp) => (cp, cp),
                None => {
                    log::warn!(
                        "glassy: font_symbol_map: invalid codepoint '{range_str}'; skipping"
                    );
                    continue;
                }
            }
        };
        out.push(SymbolMapEntry { start, end, family });
    }
    out
}

/// Parse a `U+XXXX` (or bare `XXXX` hex) codepoint string to a `u32`.
fn parse_codepoint(s: &str) -> Option<u32> {
    let hex = s
        .strip_prefix("U+")
        .or_else(|| s.strip_prefix("u+"))
        .unwrap_or(s);
    u32::from_str_radix(hex.trim(), 16).ok()
}

/// Parse a `font_variations` value into a `Vec<String>` of `"axis=value"` entries.
///
/// Accepts comma or space separation. Each token is either:
///   - `"axis=value"` (e.g. `"wght=450"`, `"wdth=75"`)
///   - A bare 4-char tag (treated as "enable", same as feature tags) — not
///     meaningful for axes; warned and kept for forward-compat.
pub fn parse_font_variations(value: &str) -> Vec<String> {
    value
        .split([',', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|token| {
            let tag = token.split('=').next().unwrap_or(token).trim();
            if tag.len() != 4 || !tag.is_ascii() {
                log::warn!(
                    "glassy: font_variations: axis tag '{tag}' is not 4 ASCII chars; skipping"
                );
            }
            token.to_string()
        })
        .collect()
}

/// Accumulated raw configuration before validation/finalization. Every field is
/// optional so the file and CLI layers can each set a subset.
#[derive(Default)]
pub(super) struct RawConfig {
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub theme: Option<String>,
    pub opacity: Option<f32>,
    pub padding: Option<f32>,
    pub padding_top: Option<f32>,
    pub padding_bottom: Option<f32>,
    pub padding_left: Option<f32>,
    pub padding_right: Option<f32>,
    pub shell: Option<Shell>,
    pub scrollback: Option<usize>,
    pub bell_visual: Option<bool>,
    pub bell_audible: Option<bool>,
    pub follow_system: Option<bool>,
    pub theme_light: Option<String>,
    pub theme_dark: Option<String>,
    pub status_bar: Option<bool>,
    pub pane_headers: Option<bool>,
    pub word_separator: Option<String>,
    pub ligatures: Option<bool>,
    pub font_features: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub restore_session: Option<bool>,
    pub copy_on_select: Option<bool>,
    pub hints_chars: Option<String>,
    pub command_badges: Option<bool>,
    pub color_fg: Option<String>,
    pub color_bg: Option<String>,
    pub color_cursor: Option<String>,
    pub color_selection_bg: Option<String>,
    pub color_ansi: Option<[Option<String>; 16]>,
    pub profiles: HashMap<String, Vec<(String, String)>>,
    pub keybinding_overrides: Vec<(String, String)>,
    // Cursor defaults (new in cursor-cfg stream)
    pub cursor_style: Option<String>,
    pub cursor_blink: Option<bool>,
    /// Path to an image file from which the theme should be auto-generated on
    /// startup (via `theme_gen`). When set, the generated theme overrides any
    /// `theme = …` setting and any `color.*` overrides.
    pub wallpaper_theme: Option<String>,
    pub cursor_trail: Option<bool>,
    pub crt_effect: Option<bool>,
    /// Window post-process effect mode (none|frosted|acrylic|crt|scanlines|grain|
    /// vignette|bloom). Supersedes the legacy `crt_effect` bool when present.
    pub window_effect: Option<String>,
    pub show_tab_bar: Option<String>,
    pub title_show_cwd: Option<bool>,
    pub title_show_count: Option<bool>,
    pub minimap: Option<bool>,
    pub quake: Option<bool>,
    pub quake_height: Option<f32>,
    pub quake_animation_ms: Option<u64>,
    pub command_history: Option<usize>,
    pub dim_unfocused: Option<bool>,
    /// Also place a rich-text (HTML) flavor on the clipboard alongside the plain
    /// text on copy, so apps that prefer HTML get a monospace-preserving paste.
    pub copy_html: Option<bool>,
    pub status_bar_segments: Option<Vec<crate::app::StatusBarSegment>>,
    pub status_bar_time_format: Option<String>,
    // --- FONTS stream additions ---
    /// Per-style font family overrides. When set, the named family is used
    /// for bold / italic / bold-italic text instead of synthesizing from the
    /// primary family. Value is a font family name or an absolute file path.
    pub font_bold: Option<String>,
    pub font_italic: Option<String>,
    pub font_bold_italic: Option<String>,
    /// Codepoint / Unicode-range → font-family routing map.
    /// Each entry is `"RANGE:Family"` where RANGE is one of:
    ///   - A single scalar:  `U+E000`
    ///   - An inclusive range: `U+E000-U+F8FF`
    ///     Example: `"U+E000-U+F8FF:Symbols Nerd Font Mono"`.
    ///     Multiple entries are separated by commas or newlines.
    pub font_symbol_map: Option<Vec<SymbolMapEntry>>,
    /// OpenType variable-font axis settings, e.g. `["wght=450", "wdth=75"]`.
    /// `wght` maps to `Weight` in cosmic-text; `wdth` maps to `Stretch`.
    /// Other axis tags are accepted in config but are currently no-ops
    /// (cosmic-text 0.19 does not expose arbitrary axis APIs at the Attrs
    /// level — they log a warning). Comma or space separated.
    pub font_variations: Option<Vec<String>>,
    pub notify_command_finish: Option<bool>,
    pub notify_command_threshold_ms: Option<u64>,
    pub command_fold: Option<bool>,
    pub power_mode: Option<bool>,
    pub power_mode_intensity: Option<f32>,
}

impl RawConfig {
    pub fn into_settings(self) -> Result<super::Settings> {
        let theme_input = self.theme.as_deref().unwrap_or("tokyo-night");
        let mut theme = color::theme_by_name(theme_input).unwrap_or_else(|| {
            log::warn!("glassy: unknown theme '{theme_input}'; using Tokyo Night");
            color::theme_by_name("tokyo-night").expect("default theme exists")
        });
        let theme_name = color::canonical_name(theme_input).to_string();

        // Apply custom color overrides if provided.
        if self.color_fg.is_some()
            || self.color_bg.is_some()
            || self.color_cursor.is_some()
            || self.color_selection_bg.is_some()
            || self.color_ansi.is_some()
        {
            if let Some(fg) = self.color_fg {
                theme.fg = parse_hex_color(&fg)?;
            }
            if let Some(bg) = self.color_bg {
                theme.bg = parse_hex_color(&bg)?;
            }
            if let Some(cursor) = self.color_cursor {
                theme.cursor = parse_hex_color(&cursor)?;
            }
            if let Some(sel_bg) = self.color_selection_bg {
                theme.selection_bg = parse_hex_color(&sel_bg)?;
            }
            if let Some(ansi_colors) = self.color_ansi {
                for (i, color_str) in ansi_colors.iter().enumerate() {
                    if let Some(color) = color_str {
                        theme.ansi16[i] = parse_hex_color(color)?;
                    }
                }
            }
        }

        // Apply wallpaper-generated theme if a path is configured.
        // This overrides all named-theme and color.* settings.
        if let Some(ref path) = self.wallpaper_theme {
            match super::theme_gen::from_image_path(path) {
                Ok(generated) => {
                    theme = generated;
                }
                Err(e) => {
                    log::warn!("glassy: wallpaper_theme '{path}' failed, using fallback: {e}");
                }
            }
        }

        let follow_system = self.follow_system.unwrap_or(false);
        let theme_dark =
            color::canonical_name(self.theme_dark.as_deref().unwrap_or(&theme_name)).to_string();
        let theme_light =
            color::canonical_name(self.theme_light.as_deref().unwrap_or("rose-pine-dawn"))
                .to_string();

        let opacity = self.opacity.unwrap_or(DEFAULT_OPACITY);
        let opacity = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            DEFAULT_OPACITY
        };
        let font_size = self.font_size.unwrap_or(DEFAULT_FONT_SIZE);
        let font_size = if font_size.is_finite() && font_size > 0.0 {
            font_size
        } else {
            DEFAULT_FONT_SIZE
        };
        // Split the `[keybindings]` overrides into single-chord binds (merged onto
        // the flat keymap) and multi-chord "leader" sequences (their own map).
        let (single_binds, key_sequences) =
            super::keymap::split_overrides(&self.keybinding_overrides);
        let config = Config {
            font_family: self.font_family,
            font_size,
            opacity,
            padding: self.padding,
            padding_top: self.padding_top,
            padding_bottom: self.padding_bottom,
            padding_left: self.padding_left,
            padding_right: self.padding_right,
            scrollback: self.scrollback.unwrap_or(DEFAULT_SCROLLBACK),
            shell: self.shell,
            bell_visual: self.bell_visual.unwrap_or(true),
            bell_audible: self.bell_audible.unwrap_or(false),
            theme: theme_name,
            follow_system,
            theme_light,
            theme_dark,
            status_bar: self.status_bar.unwrap_or(false),
            pane_headers: self.pane_headers.unwrap_or(false),
            word_separator: self.word_separator.unwrap_or_default(),
            ligatures: self.ligatures.unwrap_or(false),
            font_features: self.font_features.unwrap_or_default(),
            initial_cwd: self.cwd.filter(|s| !s.is_empty()).map(PathBuf::from),
            restore_session: self.restore_session.unwrap_or(false),
            copy_on_select: self.copy_on_select.unwrap_or(false),
            keymap: build_keymap(default_keymap(Platform::current()), &single_binds),
            key_sequences,
            cursor_style: parse_cursor_style_config(self.cursor_style.as_deref()),
            cursor_blink: self.cursor_blink.unwrap_or(false),
            wallpaper_theme: self
                .wallpaper_theme
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            cursor_trail: self.cursor_trail.unwrap_or(false),
            crt_effect: self.crt_effect.unwrap_or(false),
            // Custom-effect channel intensities [curvature, scanline, glow,
            // vignette, grain, tint]. A pleasant retro-glass default; the
            // Appearance → Custom sliders tune it live.
            custom_effect: [0.12, 0.35, 0.22, 0.30, 0.15, 0.25],
            // Resolve the window effect: an explicit `window_effect` wins; else the
            // legacy `crt_effect = true` maps to the CRT mode; else None. This keeps
            // old configs working while exposing the full mode set.
            window_effect: match self.window_effect.as_deref() {
                Some(s) => crate::renderer::WindowEffect::parse(s),
                None => {
                    if self.crt_effect == Some(true) {
                        crate::renderer::WindowEffect::Crt
                    } else {
                        crate::renderer::WindowEffect::None
                    }
                }
            },
            show_tab_bar: parse_tab_bar_mode(self.show_tab_bar.as_deref()),
            title_show_cwd: self.title_show_cwd.unwrap_or(true),
            title_show_count: self.title_show_count.unwrap_or(false),
            hints_chars: self
                .hints_chars
                .filter(|s| s.chars().filter(|c| c.is_ascii_alphabetic()).count() >= 2)
                .map(|s| s.chars().filter(|c| c.is_ascii_alphabetic()).collect()),
            command_badges: self.command_badges.unwrap_or(true),
            minimap: self.minimap.unwrap_or(false),
            quake: self.quake.unwrap_or(false),
            quake_height: {
                let h = self.quake_height.unwrap_or(0.5);
                if h.is_finite() && (0.1..=1.0).contains(&h) {
                    h
                } else {
                    0.5
                }
            },
            quake_animation_ms: self.quake_animation_ms.unwrap_or(180).min(5_000),
            command_history: self.command_history.unwrap_or(DEFAULT_COMMAND_HISTORY),
            dim_unfocused: self.dim_unfocused.unwrap_or(true),
            copy_html: self.copy_html.unwrap_or(false),
            status_bar_segments: self.status_bar_segments,
            status_bar_time_format: self
                .status_bar_time_format
                .unwrap_or_else(|| "%H:%M".to_string()),
            // FONTS stream
            font_bold: self.font_bold,
            font_italic: self.font_italic,
            font_bold_italic: self.font_bold_italic,
            font_symbol_map: self.font_symbol_map.unwrap_or_default(),
            font_variations: self.font_variations.unwrap_or_default(),
            notify_command_finish: self.notify_command_finish.unwrap_or(true),
            notify_command_threshold_ms: self
                .notify_command_threshold_ms
                .unwrap_or(DEFAULT_NOTIFY_COMMAND_THRESHOLD_MS),
            command_fold: self.command_fold.unwrap_or(true),
            power_mode: self.power_mode.unwrap_or(false),
            power_mode_intensity: {
                let i = self.power_mode_intensity.unwrap_or(0.6);
                if i.is_finite() {
                    i.clamp(0.0, 1.0)
                } else {
                    0.6
                }
            },
        };

        Ok(super::Settings { config, theme })
    }

    /// Apply the named profile's key/value pairs over the base config, returning an
    /// error if the profile is unknown or one of its values fails to parse. Called
    /// after the file load and before CLI overrides, so the CLI still wins.
    pub fn activate_profile(&mut self, name: &str) -> Result<()> {
        let key = name.to_ascii_lowercase();
        let pairs =
            self.profiles.get(&key).cloned().with_context(|| {
                format!("unknown profile '{name}' (no [profile.{name}] section)")
            })?;
        for (k, v) in &pairs {
            apply_kv(k, v, self).with_context(|| format!("in [profile.{name}]"))?;
        }
        Ok(())
    }
}

/// The resolved config file path, honoring `$XDG_CONFIG_HOME` then `$HOME`.
/// Public so the in-app settings overlay can show + write it.
pub fn path() -> Option<PathBuf> {
    config_path()
}

/// Persist `updates` (`(key, value)` pairs) into the config file, preserving all
/// other lines, comments, and ordering. A key already present is updated in place;
/// a missing key is appended. Creates the parent directory and file if needed.
/// Used by the live settings overlay so changes survive a restart.
pub fn save(updates: &[(&str, String)]) -> Result<()> {
    let path = config_path().context("no config path (HOME/XDG unset)")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating config dir {}", dir.display()))?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let out = merge_config(&existing, updates);
    std::fs::write(&path, out).with_context(|| format!("writing config {}", path.display()))?;
    Ok(())
}

/// Merge `updates` into the text of a config file: a present key is updated in
/// place (preserving its position), a missing one is appended; comments, blank
/// lines, unmanaged keys, and ordering are preserved. Pure for unit testing.
fn merge_config(existing: &str, updates: &[(&str, String)]) -> String {
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let mut written = vec![false; updates.len()];

    for line in lines.iter_mut() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let Some((key, _)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        for (i, (k, v)) in updates.iter().enumerate() {
            if !written[i] && key == *k {
                // Strip newlines and carriage returns from value to prevent injection.
                let clean_v = v.replace(['\n', '\r'], "");
                *line = format!("{k} = {clean_v}");
                written[i] = true;
            }
        }
    }
    for (i, (k, v)) in updates.iter().enumerate() {
        if !written[i] {
            // Strip newlines and carriage returns from value to prevent injection.
            let clean_v = v.replace(['\n', '\r'], "");
            lines.push(format!("{k} = {clean_v}"));
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// The resolved config file path, honoring `$XDG_CONFIG_HOME` then `$HOME`.
/// On macOS, uses ~/Library/Application Support/glassy/glassy.conf.
/// On other platforms, honors $XDG_CONFIG_HOME then ~/.config/glassy/glassy.conf.
fn config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Application Support/glassy/glassy.conf"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
            && !xdg.is_empty()
        {
            return Some(PathBuf::from(xdg).join("glassy/glassy.conf"));
        }
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".config/glassy/glassy.conf"))
    }
}

/// Section discriminant for the config file parser.
#[derive(Clone, PartialEq)]
enum Section {
    Global,
    Profile(String),
    Keybindings,
    Unknown,
}

/// Parse a `KEY=VALUE` config file into `raw`. Blank lines and `#`/`;` comments
/// are ignored; surrounding whitespace and a single layer of matching quotes are
/// stripped from values. An unknown key is warned about but not fatal; a value
/// that fails to parse for a known key is a hard error (with the line number).
pub(super) fn parse_config_file(text: &str, raw: &mut RawConfig) -> Result<()> {
    let mut section = Section::Global;
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // Section header.
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim();
            section = if name.eq_ignore_ascii_case("keybindings") {
                Section::Keybindings
            } else if let Some(profile_name) = name.strip_prefix("profile.") {
                let n = profile_name.trim().to_ascii_lowercase();
                if n.is_empty() {
                    Section::Unknown
                } else {
                    Section::Profile(n)
                }
            } else {
                Section::Unknown
            };
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("line {}: expected KEY=VALUE, got '{line}'", i + 1);
        };
        let key = key.trim().to_ascii_lowercase();
        let value = unquote(value.trim());
        match &section {
            Section::Global => {
                apply_kv(&key, value, raw).with_context(|| format!("line {}", i + 1))?;
            }
            Section::Profile(name) => {
                raw.profiles
                    .entry(name.clone())
                    .or_default()
                    .push((key, value.to_string()));
            }
            Section::Keybindings => {
                raw.keybinding_overrides.push((key, value.to_string()));
            }
            Section::Unknown => {
                // Skip content of unrecognized sections for forward-compat.
            }
        }
    }
    Ok(())
}

/// Extract the `[profile.NAME]` section names from raw config text, lower-cased and
/// in first-seen order (the parser's `HashMap` doesn't preserve order, and the
/// runtime switcher wants a stable list). Duplicates are de-duplicated.
pub(super) fn profile_names_from_text(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            let inner = line[1..line.len() - 1].trim();
            if let Some(profile_name) = inner.strip_prefix("profile.") {
                let n = profile_name.trim().to_ascii_lowercase();
                if !n.is_empty() && !names.contains(&n) {
                    names.push(n);
                }
            }
        }
    }
    names
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

/// Parse a hex color string (with or without leading #) to an Rgb.
pub(super) fn parse_hex_color(s: &str) -> Result<alacritty_terminal::vte::ansi::Rgb> {
    let hex = s.trim_start_matches('#');
    if hex.len() != 6 {
        bail!("color must be a 6-digit hex value, got '{s}'");
    }
    let r =
        u8::from_str_radix(&hex[0..2], 16).with_context(|| format!("invalid hex color '{s}'"))?;
    let g =
        u8::from_str_radix(&hex[2..4], 16).with_context(|| format!("invalid hex color '{s}'"))?;
    let b =
        u8::from_str_radix(&hex[4..6], 16).with_context(|| format!("invalid hex color '{s}'"))?;
    Ok(alacritty_terminal::vte::ansi::Rgb { r, g, b })
}

/// Apply a single recognized `key`/`value` pair into `raw`.
pub(super) fn apply_kv(key: &str, value: &str, raw: &mut RawConfig) -> Result<()> {
    match key {
        "font_family" => {
            if !value.is_empty() {
                raw.font_family = Some(value.to_string());
            }
        }
        "font_size" => {
            raw.font_size = Some(parse_pos_f32(value, "font_size")?);
        }
        "theme" => {
            if !value.is_empty() {
                // Split-theme syntax: `theme = light:X, dark:Y` turns on
                // follow_system and pins the per-scheme themes. A bare name keeps
                // the legacy single-theme behaviour. Either half may be omitted.
                if let Some((light, dark)) = parse_split_theme(value) {
                    raw.follow_system = Some(true);
                    if let Some(l) = light {
                        raw.theme_light = Some(l);
                    }
                    if let Some(d) = dark {
                        raw.theme_dark = Some(d.clone());
                        // Seed the active theme to the dark half so a first paint
                        // before the OS scheme is known is sensible.
                        raw.theme = Some(d);
                    }
                } else {
                    raw.theme = Some(value.to_string());
                }
            }
        }
        "opacity" => {
            let o: f32 = value
                .parse()
                .with_context(|| format!("opacity: invalid number '{value}'"))?;
            raw.opacity = Some(o);
        }
        "padding" => {
            let p: f32 = value
                .parse()
                .with_context(|| format!("padding: invalid number '{value}'"))?;
            if p < 0.0 {
                bail!("padding must be >= 0, got {p}");
            }
            raw.padding = Some(p);
        }
        "padding_top" => {
            let p: f32 = value
                .parse()
                .with_context(|| format!("padding_top: invalid number '{value}'"))?;
            if p < 0.0 {
                bail!("padding_top must be >= 0, got {p}");
            }
            raw.padding_top = Some(p);
        }
        "padding_bottom" => {
            let p: f32 = value
                .parse()
                .with_context(|| format!("padding_bottom: invalid number '{value}'"))?;
            if p < 0.0 {
                bail!("padding_bottom must be >= 0, got {p}");
            }
            raw.padding_bottom = Some(p);
        }
        "padding_left" => {
            let p: f32 = value
                .parse()
                .with_context(|| format!("padding_left: invalid number '{value}'"))?;
            if p < 0.0 {
                bail!("padding_left must be >= 0, got {p}");
            }
            raw.padding_left = Some(p);
        }
        "padding_right" => {
            let p: f32 = value
                .parse()
                .with_context(|| format!("padding_right: invalid number '{value}'"))?;
            if p < 0.0 {
                bail!("padding_right must be >= 0, got {p}");
            }
            raw.padding_right = Some(p);
        }
        "shell" => {
            if let Some(shell) = parse_shell(value) {
                raw.shell = Some(shell);
            }
        }
        "scrollback" => {
            let n: usize = value
                .parse()
                .with_context(|| format!("scrollback: invalid integer '{value}'"))?;
            raw.scrollback = Some(n.clamp(0, 1_000_000));
        }
        "command_history" => {
            let n: usize = value
                .parse()
                .with_context(|| format!("command_history: invalid integer '{value}'"))?;
            raw.command_history = Some(n.clamp(0, 10_000));
        }
        "notify_command_finish" => {
            raw.notify_command_finish = Some(parse_bool(value, "notify_command_finish")?);
        }
        "notify_command_threshold_ms" => {
            let ms: u64 = value.parse().with_context(|| {
                format!("notify_command_threshold_ms: invalid integer '{value}'")
            })?;
            raw.notify_command_threshold_ms = Some(ms.min(86_400_000)); // cap at 24h
        }
        "command_fold" => {
            raw.command_fold = Some(parse_bool(value, "command_fold")?);
        }
        "bell_visual" => {
            raw.bell_visual = Some(parse_bool(value, "bell_visual")?);
        }
        "bell_audible" => {
            raw.bell_audible = Some(parse_bool(value, "bell_audible")?);
        }
        "follow_system" => {
            raw.follow_system = Some(parse_bool(value, "follow_system")?);
        }
        "theme_light" => {
            if !value.is_empty() {
                raw.theme_light = Some(value.to_string());
            }
        }
        "theme_dark" => {
            if !value.is_empty() {
                raw.theme_dark = Some(value.to_string());
            }
        }
        "status_bar" => {
            raw.status_bar = Some(parse_bool(value, "status_bar")?);
        }
        "pane_headers" => {
            raw.pane_headers = Some(parse_bool(value, "pane_headers")?);
        }
        "dim_unfocused" => {
            raw.dim_unfocused = Some(parse_bool(value, "dim_unfocused")?);
        }
        "word_separator" => {
            raw.word_separator = Some(value.to_string());
        }
        "ligatures" => {
            raw.ligatures = Some(parse_bool(value, "ligatures")?);
        }
        "font_features" => {
            let features: Vec<String> = value
                .split([',', ' '])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|token| {
                    let tag = token.split('=').next().unwrap_or(token).trim();
                    if tag.len() != 4 || !tag.is_ascii() {
                        log::warn!(
                            "glassy: ignoring invalid font_features entry '{}' \
                             (tag must be exactly 4 ASCII characters)",
                            token
                        );
                    }
                    token.to_string()
                })
                .collect();
            raw.font_features = Some(features);
        }
        "cwd" => {
            if !value.is_empty() {
                raw.cwd = Some(value.to_string());
            }
        }
        "restore_session" => {
            raw.restore_session = Some(parse_bool(value, "restore_session")?);
        }
        "copy_on_select" => {
            raw.copy_on_select = Some(parse_bool(value, "copy_on_select")?);
        }
        "copy_html" => {
            raw.copy_html = Some(parse_bool(value, "copy_html")?);
        }
        "cursor_trail" => {
            raw.cursor_trail = Some(parse_bool(value, "cursor_trail")?);
        }
        "crt_effect" => {
            raw.crt_effect = Some(parse_bool(value, "crt_effect")?);
        }
        "window_effect" => {
            // Accept any of the mode words (or bool-ish spellings, which migrate
            // from `crt_effect`). Unknown strings are tolerated and resolve to
            // `none` at finalization, so a typo never aborts config load.
            let v = value.to_ascii_lowercase();
            match v.as_str() {
                "none" | "off" | "false" | "no" | "0" | "frosted" | "frost" | "acrylic" | "crt"
                | "true" | "on" | "yes" | "1" | "scanlines" | "scanline" | "scan" | "grain"
                | "noise" | "film" | "vignette" | "vig" | "bloom" | "glow" => {
                    raw.window_effect = Some(v);
                }
                _ => bail!(
                    "window_effect must be one of none/frosted/acrylic/crt/scanlines/grain/\
                     vignette/bloom, got '{value}'"
                ),
            }
        }
        "show_tab_bar" => {
            // Accepts the three policy words plus the usual bool spellings
            // (true→always, false→never) so a boolean reads naturally too.
            let v = value.to_ascii_lowercase();
            match v.as_str() {
                "auto" | "always" | "never" => raw.show_tab_bar = Some(v),
                "true" | "yes" | "on" | "1" => raw.show_tab_bar = Some("always".into()),
                "false" | "no" | "off" | "0" => raw.show_tab_bar = Some("never".into()),
                _ => bail!("show_tab_bar must be auto/always/never, got '{value}'"),
            }
        }
        "title_show_cwd" => {
            raw.title_show_cwd = Some(parse_bool(value, "title_show_cwd")?);
        }
        "title_show_count" => {
            raw.title_show_count = Some(parse_bool(value, "title_show_count")?);
        }
        "hints_chars" => {
            // The label alphabet for hints mode (home-row-first letters). Only the
            // ASCII letters are kept; an alphabet shorter than 2 chars is ignored.
            raw.hints_chars = Some(value.to_string());
        }
        "command_badges" => {
            raw.command_badges = Some(parse_bool(value, "command_badges")?);
        }
        "minimap" => {
            raw.minimap = Some(parse_bool(value, "minimap")?);
        }
        "quake" => {
            raw.quake = Some(parse_bool(value, "quake")?);
        }
        "quake_height" => {
            let h: f32 = value
                .parse()
                .with_context(|| format!("quake_height: invalid number '{value}'"))?;
            if !(h.is_finite() && (0.1..=1.0).contains(&h)) {
                bail!("quake_height must be between 0.1 and 1.0, got {h}");
            }
            raw.quake_height = Some(h);
        }
        "quake_animation_ms" => {
            let ms: u64 = value
                .parse()
                .with_context(|| format!("quake_animation_ms: invalid integer '{value}'"))?;
            raw.quake_animation_ms = Some(ms.min(5_000));
        }
        "power_mode" => {
            raw.power_mode = Some(parse_bool(value, "power_mode")?);
        }
        "power_mode_intensity" => {
            let i: f32 = value
                .parse()
                .with_context(|| format!("power_mode_intensity: invalid number '{value}'"))?;
            if !(i.is_finite() && (0.0..=1.0).contains(&i)) {
                bail!("power_mode_intensity must be between 0.0 and 1.0, got {i}");
            }
            raw.power_mode_intensity = Some(i);
        }
        "color.fg" => {
            parse_hex_color(value)?;
            raw.color_fg = Some(value.to_string());
        }
        "color.bg" => {
            parse_hex_color(value)?;
            raw.color_bg = Some(value.to_string());
        }
        "color.cursor" => {
            parse_hex_color(value)?;
            raw.color_cursor = Some(value.to_string());
        }
        "color.selection_bg" => {
            parse_hex_color(value)?;
            raw.color_selection_bg = Some(value.to_string());
        }
        k if k.starts_with("color.ansi") => {
            let ansi_idx = k
                .strip_prefix("color.ansi")
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&idx| idx < 16);
            if let Some(idx) = ansi_idx {
                parse_hex_color(value)?;
                if raw.color_ansi.is_none() {
                    raw.color_ansi = Some(Default::default());
                }
                if let Some(ref mut ansi) = raw.color_ansi {
                    ansi[idx] = Some(value.to_string());
                }
            } else {
                log::warn!("glassy: ignoring invalid color key '{k}'");
            }
        }
        "cursor_style" => {
            let lower = value.to_ascii_lowercase();
            match lower.as_str() {
                "block" | "beam" | "underline" => {
                    raw.cursor_style = Some(lower);
                }
                _ => {
                    bail!("cursor_style must be block, beam, or underline; got '{value}'");
                }
            }
        }
        "cursor_blink" => {
            raw.cursor_blink = Some(parse_bool(value, "cursor_blink")?);
        }
        "wallpaper_theme" => {
            if !value.is_empty() {
                raw.wallpaper_theme = Some(value.to_string());
            }
        }
        "status_bar_segments" => {
            use crate::app::StatusBarSegment;
            if value.is_empty() {
                raw.status_bar_segments = None;
            } else {
                let segs: Vec<StatusBarSegment> = value
                    .split([',', ' '])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| match s.to_ascii_lowercase().as_str() {
                        "cwd" => Some(StatusBarSegment::Cwd),
                        "git_branch" | "git" => Some(StatusBarSegment::GitBranch),
                        "process" | "fg_process" => Some(StatusBarSegment::Process),
                        "time" | "clock" => Some(StatusBarSegment::Time),
                        "mode" => Some(StatusBarSegment::Mode),
                        "broadcast" | "bcast" => Some(StatusBarSegment::Broadcast),
                        "selection" | "sel" => Some(StatusBarSegment::Selection),
                        "scroll" => Some(StatusBarSegment::Scroll),
                        "encoding" | "enc" => Some(StatusBarSegment::Encoding),
                        "progress" => Some(StatusBarSegment::Progress),
                        "exit_status" | "exit" => Some(StatusBarSegment::ExitStatus),
                        "key_hints" | "hints" => Some(StatusBarSegment::KeyHints),
                        other => {
                            log::warn!(
                                "glassy: ignoring unknown status_bar_segments entry '{other}'"
                            );
                            None
                        }
                    })
                    .collect();
                raw.status_bar_segments = Some(segs);
            }
        }
        "status_bar_time_format" => {
            if !value.is_empty() {
                raw.status_bar_time_format = Some(value.to_string());
            }
        }
        // --- FONTS stream ---
        "font_bold" => {
            if !value.is_empty() {
                raw.font_bold = Some(value.to_string());
            }
        }
        "font_italic" => {
            if !value.is_empty() {
                raw.font_italic = Some(value.to_string());
            }
        }
        "font_bold_italic" => {
            if !value.is_empty() {
                raw.font_bold_italic = Some(value.to_string());
            }
        }
        "font_symbol_map" => {
            if !value.is_empty() {
                raw.font_symbol_map = Some(parse_symbol_map(value));
            }
        }
        "font_variations" => {
            if !value.is_empty() {
                raw.font_variations = Some(parse_font_variations(value));
            }
        }
        other => {
            log::warn!("glassy: ignoring unknown config key '{other}'");
        }
    }
    Ok(())
}

/// Parse a cursor style string into the app config enum (block is the default).
pub(crate) fn parse_cursor_style_config(s: Option<&str>) -> crate::app::CursorStyleConfig {
    match s {
        Some("beam") => crate::app::CursorStyleConfig::Beam,
        Some("underline") => crate::app::CursorStyleConfig::Underline,
        _ => crate::app::CursorStyleConfig::Block,
    }
}

/// Parse a boolean for a named field, accepting the usual spellings.
pub(super) fn parse_bool(value: &str, field: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => bail!("{field} must be true/false (or yes/no, on/off, 1/0), got '{value}'"),
    }
}

/// Parse a strictly-positive float for a named field.
pub(super) fn parse_pos_f32(value: &str, field: &str) -> Result<f32> {
    let v: f32 = value
        .parse()
        .with_context(|| format!("{field}: invalid number '{value}'"))?;
    if !(v.is_finite() && v > 0.0) {
        bail!("{field} must be a positive number, got {value}");
    }
    Ok(v)
}

/// Map a (validated) `show_tab_bar` token to the [`TabBarMode`] enum, defaulting
/// to `Auto` when unset. The validation already happened in [`apply_kv`].
pub(super) fn parse_tab_bar_mode(value: Option<&str>) -> crate::app::TabBarMode {
    use crate::app::TabBarMode;
    match value {
        Some("always") => TabBarMode::Always,
        Some("never") => TabBarMode::Never,
        _ => TabBarMode::Auto,
    }
}

/// Parse the split-theme syntax `light:NAME, dark:NAME` (either half optional,
/// order-independent, comma-separated). Returns `Some((light, dark))` only when at
/// least one `light:`/`dark:` token is present; a bare theme name returns `None`
/// so the caller falls back to the single-theme path. Whitespace around tokens and
/// names is tolerated.
pub(super) fn parse_split_theme(value: &str) -> Option<(Option<String>, Option<String>)> {
    let mut light = None;
    let mut dark = None;
    let mut saw_tag = false;
    for part in value.split(',') {
        let part = part.trim();
        if let Some(rest) = part
            .strip_prefix("light:")
            .or_else(|| part.strip_prefix("Light:"))
        {
            let n = rest.trim();
            if !n.is_empty() {
                light = Some(n.to_string());
            }
            saw_tag = true;
        } else if let Some(rest) = part
            .strip_prefix("dark:")
            .or_else(|| part.strip_prefix("Dark:"))
        {
            let n = rest.trim();
            if !n.is_empty() {
                dark = Some(n.to_string());
            }
            saw_tag = true;
        }
    }
    saw_tag.then_some((light, dark))
}

/// Split a `shell` value (a whitespace-separated program + args) into a `Shell`.
/// Returns `None` for an empty value.
pub(super) fn parse_shell(value: &str) -> Option<Shell> {
    let mut parts = value.split_whitespace();
    let program = parts.next()?.to_string();
    let args = parts.map(str::to_string).collect();
    Some(Shell::new(program, args))
}
